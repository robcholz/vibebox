use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const INSTANCE_DIR_NAME: &str = ".vibebox";
pub const GLOBAL_CACHE_DIR_NAME: &str = "vibebox";
pub const GLOBAL_DIR_NAME: &str = ".vibebox";
pub const GLOBAL_SESSION_FILENAME: &str = "session.toml";
pub const SESSION_TEMP_PREFIX: &str = "sessions";
pub const SESSION_TOML_SUFFIX: &str = ".toml";
use crate::config::CONFIG_FILENAME;
pub const INSTANCE_TOML_FILENAME: &str = "instance.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub directory: PathBuf,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub last_active: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GlobalSessionIndex {
    #[serde(default)]
    directories: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct InstanceMetadata {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    last_active: Option<String>,
}

#[derive(Debug)]
pub struct SessionManager {
    global_dir: PathBuf,
    session_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("HOME environment variable is not set")]
    MissingHome,
    #[error("Session directory must be absolute: {0}")]
    NonAbsoluteDirectory(PathBuf),
    #[error("Session directory does not exist: {0}")]
    MissingDirectory(PathBuf),
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
        let session_path = global_dir.join(GLOBAL_SESSION_FILENAME);
        Self {
            global_dir,
            session_path,
        }
    }

    pub fn index_path(&self) -> &Path {
        &self.session_path
    }

    pub fn update_global_sessions(&self, directory: &Path) -> Result<Vec<PathBuf>, SessionError> {
        let directory = self.normalize_directory(directory)?;
        let mut index = self.read_global_index()?;
        let mut changed = false;

        let removed = prune_invalid_dirs(&mut index);
        if removed > 0 {
            changed = true;
        }

        if is_vibebox_dir(&directory) && !index.directories.iter().any(|dir| dir == &directory) {
            index.directories.push(directory);
            changed = true;
        }

        if changed {
            self.write_global_index(&index)?;
        }

        Ok(index.directories)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>, SessionError> {
        let mut index = self.read_global_index()?;
        let removed = prune_invalid_dirs(&mut index);
        if removed > 0 {
            self.write_global_index(&index)?;
        }
        let mut sessions = Vec::with_capacity(index.directories.len());
        for directory in index.directories {
            let meta = read_instance_metadata(&directory)?;
            sessions.push(SessionRecord {
                directory,
                id: meta.id,
                last_active: meta.last_active,
            });
        }
        Ok(sessions)
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

    fn read_global_index(&self) -> Result<GlobalSessionIndex, SessionError> {
        if !self.session_path.exists() {
            return Ok(GlobalSessionIndex::default());
        }
        let content = fs::read_to_string(&self.session_path)?;
        Ok(toml::from_str(&content)?)
    }

    fn write_global_index(&self, index: &GlobalSessionIndex) -> Result<(), SessionError> {
        fs::create_dir_all(&self.global_dir)?;
        let content = toml::to_string_pretty(index)?;
        atomic_write(&self.session_path, content.as_bytes())?;
        Ok(())
    }
}

fn is_vibebox_dir(directory: &Path) -> bool {
    if !directory.is_absolute() {
        return false;
    }
    directory.join(CONFIG_FILENAME).is_file()
}

fn prune_invalid_dirs(index: &mut GlobalSessionIndex) -> usize {
    let before = index.directories.len();
    index.directories.retain(|dir| is_vibebox_dir(dir));
    before - index.directories.len()
}

fn read_instance_metadata(directory: &Path) -> Result<InstanceMetadata, SessionError> {
    let instance_path = directory
        .join(INSTANCE_DIR_NAME)
        .join(INSTANCE_TOML_FILENAME);
    if !instance_path.exists() {
        return Ok(InstanceMetadata::default());
    }
    let raw = fs::read_to_string(&instance_path)?;
    let mut meta: InstanceMetadata = toml::from_str(&raw)?;
    if let Some(id) = &meta.id {
        if id.trim().is_empty() {
            meta.id = None;
        }
    }
    if let Some(last_active) = &meta.last_active {
        if last_active.trim().is_empty() {
            meta.last_active = None;
        }
    }
    Ok(meta)
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

    fn manager(temp: &TempDir) -> SessionManager {
        SessionManager::with_global_dir(temp.path().join("global"))
    }

    fn create_project_dir(temp: &TempDir) -> PathBuf {
        let dir = temp.path().join("project");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn update_global_sessions_adds_directory() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(project_dir.join(VIBEBOX_CONFIG_FILENAME), "").unwrap();

        let dirs = mgr.update_global_sessions(&project_dir).unwrap();

        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], project_dir.canonicalize().unwrap());
        assert!(mgr.index_path().exists());
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
    fn update_global_sessions_removes_missing_vibebox_toml() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(project_dir.join(VIBEBOX_CONFIG_FILENAME), "").unwrap();

        let _ = mgr.update_global_sessions(&project_dir).unwrap();
        fs::remove_file(project_dir.join(VIBEBOX_CONFIG_FILENAME)).unwrap();

        let dirs = mgr.update_global_sessions(&project_dir).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn list_sessions_reads_instance_metadata() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(project_dir.join(VIBEBOX_CONFIG_FILENAME), "").unwrap();

        let instance_dir = project_dir.join(INSTANCE_DIR_NAME);
        fs::create_dir_all(&instance_dir).unwrap();
        fs::write(
            instance_dir.join(INSTANCE_TOML_FILENAME),
            "id = \"019bf290-cccc-7c23-ba1d-dce7e6d40693\"\nlast_active = \"2026-02-07T05:00:00Z\"\n",
        )
        .unwrap();

        let _ = mgr.update_global_sessions(&project_dir).unwrap();
        let sessions = mgr.list_sessions().unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].id.as_deref(),
            Some("019bf290-cccc-7c23-ba1d-dce7e6d40693")
        );
        assert_eq!(
            sessions[0].last_active.as_deref(),
            Some("2026-02-07T05:00:00Z")
        );
    }
}
