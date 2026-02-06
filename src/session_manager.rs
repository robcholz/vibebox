use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub const INSTANCE_DIR_NAME: &str = ".vibebox";
pub const GLOBAL_DIR_NAME: &str = ".vibebox";
pub const SESSION_INDEX_FILENAME: &str = "sessions.toml";
pub const SESSION_TEMP_PREFIX: &str = "sessions";
pub const SESSION_TOML_SUFFIX: &str = ".toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: Uuid,
    pub directory: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub last_active: OffsetDateTime,
    #[serde(default)]
    pub ref_count: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionIndex {
    #[serde(default)]
    sessions: Vec<SessionRecord>,
}

#[derive(Debug)]
pub struct SessionManager {
    global_dir: PathBuf,
    index_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("HOME environment variable is not set")]
    MissingHome,
    #[error("Session directory must be absolute: {0}")]
    NonAbsoluteDirectory(PathBuf),
    #[error("Session directory does not exist: {0}")]
    MissingDirectory(PathBuf),
    #[error("Session already exists for directory: {0}")]
    DirectoryAlreadyHasSession(PathBuf),
    #[error("Session not found: {0}")]
    SessionNotFound(Uuid),
    #[error("Ref count underflow for session: {0}")]
    RefCountUnderflow(Uuid),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSer(#[from] toml::ser::Error),
}

impl SessionManager {
    pub fn new() -> Result<Self, SessionError> {
        let home = env::var_os("HOME").ok_or(SessionError::MissingHome)?;
        Ok(Self::with_global_dir(
            PathBuf::from(home).join(GLOBAL_DIR_NAME),
        ))
    }

    pub fn with_global_dir(global_dir: PathBuf) -> Self {
        let index_path = global_dir.join(SESSION_INDEX_FILENAME);
        Self {
            global_dir,
            index_path,
        }
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    pub fn create_session(&self, directory: &Path) -> Result<SessionRecord, SessionError> {
        let directory = self.normalize_directory(directory)?;
        let mut index = self.read_index()?;

        if index.sessions.iter().any(|s| s.directory == directory) {
            return Err(SessionError::DirectoryAlreadyHasSession(directory));
        }

        let now = OffsetDateTime::now_utc();
        let session = SessionRecord {
            id: Uuid::now_v7(),
            directory: directory.clone(),
            last_active: now,
            ref_count: 1,
        };

        fs::create_dir_all(self.instance_dir_for(&directory))?;
        index.sessions.push(session.clone());
        self.write_index(&index)?;

        Ok(session)
    }

    pub fn get_or_create_session(&self, directory: &Path) -> Result<SessionRecord, SessionError> {
        let directory = self.normalize_directory(directory)?;
        let mut index = self.read_index()?;

        if let Some(pos) = index.sessions.iter().position(|s| s.directory == directory) {
            if !self
                .instance_dir_for(&index.sessions[pos].directory)
                .is_dir()
            {
                index.sessions.remove(pos);
            } else {
                let now = OffsetDateTime::now_utc();
                let session = &mut index.sessions[pos];
                session.ref_count = session.ref_count.saturating_add(1);
                session.last_active = now;
                let updated = session.clone();
                self.write_index(&index)?;
                return Ok(updated);
            }
        }

        let now = OffsetDateTime::now_utc();
        let session = SessionRecord {
            id: Uuid::now_v7(),
            directory: directory.clone(),
            last_active: now,
            ref_count: 1,
        };

        fs::create_dir_all(self.instance_dir_for(&directory))?;
        index.sessions.push(session.clone());
        self.write_index(&index)?;

        Ok(session)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>, SessionError> {
        let mut index = self.read_index()?;
        let removed = self.remove_orphans(&mut index);
        if removed > 0 {
            self.write_index(&index)?;
        }
        Ok(index.sessions)
    }

    pub fn delete_session(&self, id: Uuid) -> Result<bool, SessionError> {
        let mut index = self.read_index()?;
        let Some(pos) = index.sessions.iter().position(|s| s.id == id) else {
            return Ok(false);
        };

        let session = index.sessions.remove(pos);
        let instance_dir = self.instance_dir_for(&session.directory);
        match fs::remove_dir_all(&instance_dir) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }

        self.write_index(&index)?;
        Ok(true)
    }

    pub fn bump_last_active(&self, id: Uuid) -> Result<SessionRecord, SessionError> {
        let now = OffsetDateTime::now_utc();
        self.update_session(id, |session| {
            session.last_active = now;
            Ok(())
        })
    }

    pub fn increment_ref_count(&self, id: Uuid) -> Result<SessionRecord, SessionError> {
        let now = OffsetDateTime::now_utc();
        self.update_session(id, |session| {
            session.ref_count = session.ref_count.saturating_add(1);
            session.last_active = now;
            Ok(())
        })
    }

    pub fn decrement_ref_count(&self, id: Uuid) -> Result<SessionRecord, SessionError> {
        let now = OffsetDateTime::now_utc();
        self.update_session(id, |session| {
            if session.ref_count == 0 {
                return Err(SessionError::RefCountUnderflow(id));
            }
            session.ref_count -= 1;
            session.last_active = now;
            Ok(())
        })
    }

    pub fn cleanup_orphans(&self) -> Result<usize, SessionError> {
        let mut index = self.read_index()?;
        let removed = self.remove_orphans(&mut index);
        if removed > 0 {
            self.write_index(&index)?;
        }
        Ok(removed)
    }

    fn update_session<F>(&self, id: Uuid, mut update: F) -> Result<SessionRecord, SessionError>
    where
        F: FnMut(&mut SessionRecord) -> Result<(), SessionError>,
    {
        let mut index = self.read_index()?;
        let Some(pos) = index.sessions.iter().position(|s| s.id == id) else {
            return Err(SessionError::SessionNotFound(id));
        };

        if !self
            .instance_dir_for(&index.sessions[pos].directory)
            .is_dir()
        {
            index.sessions.remove(pos);
            self.write_index(&index)?;
            return Err(SessionError::SessionNotFound(id));
        }

        update(&mut index.sessions[pos])?;
        let updated = index.sessions[pos].clone();
        self.write_index(&index)?;
        Ok(updated)
    }

    fn normalize_directory(&self, directory: &Path) -> Result<PathBuf, SessionError> {
        if !directory.is_absolute() {
            return Err(SessionError::NonAbsoluteDirectory(directory.to_path_buf()));
        }
        if !directory.exists() {
            return Err(SessionError::MissingDirectory(directory.to_path_buf()));
        }
        Ok(directory.canonicalize()?)
    }

    fn instance_dir_for(&self, directory: &Path) -> PathBuf {
        directory.join(INSTANCE_DIR_NAME)
    }

    fn remove_orphans(&self, index: &mut SessionIndex) -> usize {
        let before = index.sessions.len();
        index
            .sessions
            .retain(|s| self.instance_dir_for(&s.directory).is_dir());
        before - index.sessions.len()
    }

    fn read_index(&self) -> Result<SessionIndex, SessionError> {
        if !self.index_path.exists() {
            return Ok(SessionIndex::default());
        }
        let content = fs::read_to_string(&self.index_path)?;
        Ok(toml::from_str(&content)?)
    }

    fn write_index(&self, index: &SessionIndex) -> Result<(), SessionError> {
        fs::create_dir_all(&self.global_dir)?;
        let content = toml::to_string_pretty(index)?;
        atomic_write(&self.index_path, content.as_bytes())?;
        Ok(())
    }
}

fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ));
    };

    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(SESSION_TEMP_PREFIX)
        .suffix(SESSION_TOML_SUFFIX)
        .tempfile_in(parent)?;
    temp.write_all(content)?;
    temp.flush()?;
    temp.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use time::OffsetDateTime;

    fn manager(temp: &TempDir) -> SessionManager {
        SessionManager::with_global_dir(temp.path().join("global"))
    }

    fn create_project_dir(temp: &TempDir) -> PathBuf {
        let dir = temp.path().join("project");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_session_writes_index_and_instance_dir() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();

        assert!(mgr.index_path().exists());
        assert_eq!(session.directory, project_dir.canonicalize().unwrap());
        assert_eq!(session.ref_count, 1);
        assert!(project_dir.join(INSTANCE_DIR_NAME).is_dir());
    }

    #[test]
    fn create_session_rejects_non_absolute_directory() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);

        let err = mgr.create_session(Path::new("relative/path")).unwrap_err();

        assert!(matches!(err, SessionError::NonAbsoluteDirectory(_)));
    }

    #[test]
    fn create_session_rejects_missing_directory() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let missing = temp.path().join("missing");

        let err = mgr.create_session(&missing).unwrap_err();

        assert!(matches!(err, SessionError::MissingDirectory(_)));
    }

    #[test]
    fn create_session_rejects_duplicate_directory() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let _session = mgr.create_session(&project_dir).unwrap();
        let err = mgr.create_session(&project_dir).unwrap_err();

        assert!(matches!(err, SessionError::DirectoryAlreadyHasSession(_)));
    }

    #[test]
    fn get_or_create_increments_ref_count_for_existing_session() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let first = mgr.create_session(&project_dir).unwrap();
        let second = mgr.get_or_create_session(&project_dir).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.ref_count, 2);
    }

    #[test]
    fn decrement_ref_count_errors_on_underflow() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        let session = mgr.decrement_ref_count(session.id).unwrap();
        assert_eq!(session.ref_count, 0);

        let err = mgr.decrement_ref_count(session.id).unwrap_err();
        assert!(matches!(err, SessionError::RefCountUnderflow(_)));
    }

    #[test]
    fn list_sessions_cleans_orphans() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let _session = mgr.create_session(&project_dir).unwrap();
        fs::remove_dir_all(project_dir.join(INSTANCE_DIR_NAME)).unwrap();

        let sessions = mgr.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn delete_session_removes_instance_dir_and_index() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        let removed = mgr.delete_session(session.id).unwrap();

        assert!(removed);
        assert!(!project_dir.join(INSTANCE_DIR_NAME).exists());
        assert!(mgr.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn invalid_toml_returns_error() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let index_path = mgr.index_path();
        fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        fs::write(index_path, "this is not toml").unwrap();

        let err = mgr.list_sessions().unwrap_err();

        assert!(matches!(err, SessionError::TomlDe(_)));
    }

    #[test]
    fn bump_last_active_updates_timestamp() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        let before = session.last_active;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let updated = mgr.bump_last_active(session.id).unwrap();
        let now = OffsetDateTime::now_utc();

        assert!(updated.last_active >= before);
        assert!(updated.last_active <= now);
    }

    #[test]
    fn cleanup_orphans_returns_removed_count() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let _session = mgr.create_session(&project_dir).unwrap();
        fs::remove_dir_all(project_dir.join(INSTANCE_DIR_NAME)).unwrap();

        let removed = mgr.cleanup_orphans().unwrap();
        assert_eq!(removed, 1);
        assert!(mgr.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn increment_ref_count_updates_last_active() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let updated = mgr.increment_ref_count(session.id).unwrap();

        assert_eq!(updated.ref_count, session.ref_count + 1);
        assert!(updated.last_active >= session.last_active);
    }

    #[test]
    fn decrement_ref_count_updates_last_active() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let updated = mgr.decrement_ref_count(session.id).unwrap();

        assert_eq!(updated.ref_count, session.ref_count - 1);
        assert!(updated.last_active >= session.last_active);
    }

    #[test]
    fn list_sessions_returns_active_sessions() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);

        let session = mgr.create_session(&project_dir).unwrap();
        let sessions = mgr.list_sessions().unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id);
    }
}
