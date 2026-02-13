use anyhow::Result;
use std::{
    env, fs,
    io::{self, Write},
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};

use crate::config::config_path;
use crate::instance::read_instance_config;
use crate::utils::pid_is_alive;
use serde::{Deserialize, Serialize};

pub const INSTANCE_DIR_NAME: &str = ".vibebox";
pub const GLOBAL_CACHE_DIR_NAME: &str = "vibebox";
pub const GLOBAL_DIR_NAME: &str = ".vibebox";
const SESSION_TOML_SUFFIX: &str = ".toml";
pub const VM_MANAGER_SOCKET_NAME: &str = "vm.sock";
pub const VM_MANAGER_PID_NAME: &str = "vm.pid";
const SESSIONS_DIR_NAME: &str = "sessions";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub directory: PathBuf,
    pub id: String,
    pub last_active: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionEntry {
    pub directory: PathBuf,
    pub id: String,
}

#[derive(Debug)]
pub struct SessionManager {
    sessions_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanSummary {
    pub instance_dir: PathBuf,
    pub removed_instance_dir: bool,
    pub removed_sessions: usize,
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
    Io(#[from] io::Error),
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
        let sessions_dir = global_dir.join(SESSIONS_DIR_NAME);
        Self { sessions_dir }
    }

    pub fn index_path(&self) -> &Path {
        &self.sessions_dir
    }

    pub fn update_global_sessions(&self, directory: &Path) -> Result<Vec<PathBuf>, SessionError> {
        let directory = self.normalize_directory(directory)?;
        fs::create_dir_all(&self.sessions_dir)?;

        let (mut sessions, removed) = self.prune_stale_sessions()?;
        let has_config = is_vibebox_dir(&directory);
        let mut added = false;

        if has_config {
            let id = read_instance_config(&directory).map_or(None, |config| Some(config.id));
            if let Some(id) = id {
                let record = SessionEntry {
                    directory: directory.clone(),
                    id: id.clone(),
                };
                self.write_session_record(&record)?;
                if let Some(existing) = sessions.iter_mut().find(|s| s.id == id) {
                    *existing = record;
                } else {
                    sessions.push(record);
                }
                added = true;
            } else {
                tracing::warn!(
                    directory = %directory.display(),
                    "missing session id in instance file"
                );
            }
        }

        if removed > 0 || added {
            tracing::debug!(
                path = %self.sessions_dir.display(),
                removed,
                added,
                entries = sessions.len(),
                "updated global sessions"
            );
        } else {
            tracing::debug!(
                path = %self.sessions_dir.display(),
                entries = sessions.len(),
                has_config,
                "global sessions unchanged"
            );
        }

        Ok(sessions.into_iter().map(|s| s.directory).collect())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>, SessionError> {
        let (sessions, removed) = self.prune_stale_sessions()?;
        if removed > 0 {
            tracing::info!(
                path = %self.sessions_dir.display(),
                removed,
                entries = sessions.len(),
                "pruned stale sessions"
            );
        }
        let mut records = Vec::with_capacity(sessions.len());
        for session in sessions {
            let last_active =
                read_instance_config(&session.directory).map_or(None, |option| option.last_active);
            let active = is_session_active(&session.directory);
            records.push(SessionRecord {
                directory: session.directory,
                id: session.id,
                last_active,
                active,
            });
        }
        Ok(records)
    }

    pub fn clean_project(&self, directory: &Path) -> Result<CleanSummary, SessionError> {
        let directory = self.normalize_directory(directory)?;
        let instance_dir = directory.join(INSTANCE_DIR_NAME);
        let mut removed_instance_dir = false;
        if instance_dir.exists() {
            fs::remove_dir_all(&instance_dir)?;
            removed_instance_dir = true;
        }
        let removed_sessions = self.remove_session_records_for_directory(&directory)?;
        Ok(CleanSummary {
            instance_dir,
            removed_instance_dir,
            removed_sessions,
        })
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

    fn session_path_for(&self, id: &str) -> PathBuf {
        let filename = format!("{id}{SESSION_TOML_SUFFIX}");
        self.sessions_dir.join(filename)
    }

    fn write_session_record(&self, record: &SessionEntry) -> Result<(), SessionError> {
        fs::create_dir_all(&self.sessions_dir)?;
        let path = self.session_path_for(&record.id);
        let content = toml::to_string_pretty(record)?;
        atomic_write(&path, content.as_bytes())?;
        tracing::debug!(
            path = %path.display(),
            "wrote session record"
        );
        Ok(())
    }

    fn prune_stale_sessions(&self) -> Result<(Vec<SessionEntry>, usize), SessionError> {
        if !self.sessions_dir.exists() {
            return Ok((Vec::new(), 0));
        }

        let mut sessions = Vec::new();
        let mut removed = 0usize;

        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let record = read_session_file(&path)?;
            if !is_vibebox_dir(&record.directory) {
                let _ = fs::remove_file(&path);
                removed += 1;
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && stem != record.id
            {
                let _ = fs::remove_file(&path);
                removed += 1;
                continue;
            }
            sessions.push(record);
        }

        Ok((sessions, removed))
    }

    fn remove_session_records_for_directory(
        &self,
        directory: &Path,
    ) -> Result<usize, SessionError> {
        if !self.sessions_dir.exists() {
            return Ok(0);
        }
        let mut removed = 0usize;
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let record = read_session_file(&path)?;
            if record.directory == directory {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(
                directory = %directory.display(),
                removed,
                "removed session records"
            );
        }
        Ok(removed)
    }
}

fn is_vibebox_dir(directory: &Path) -> bool {
    if !directory.is_absolute() {
        return false;
    }
    config_path(directory).is_file()
}

fn is_session_active(directory: &Path) -> bool {
    let instance_dir = directory.join(INSTANCE_DIR_NAME);
    let pid_path = instance_dir.join(VM_MANAGER_PID_NAME);
    let socket_path = instance_dir.join(VM_MANAGER_SOCKET_NAME);

    let pid = read_pid(&pid_path);
    let is_alive = pid.map(pid_is_alive).unwrap_or(false);
    if !is_alive {
        let _ = fs::remove_file(&pid_path);
        return false;
    }

    if let Ok(metadata) = fs::metadata(&socket_path) {
        return metadata.file_type().is_socket();
    }

    true
}

fn read_pid(path: &Path) -> Option<u32> {
    let content = fs::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

fn read_session_file(path: &Path) -> Result<SessionEntry, SessionError> {
    let raw = fs::read_to_string(path)?;
    let record: SessionEntry = toml::from_str(&raw)?;
    if record.id.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "session id missing").into());
    }
    Ok(record)
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
        .prefix(SESSIONS_DIR_NAME)
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
    use crate::instance::{InstanceConfig, write_instance_config};
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

    fn write_instance(project_dir: &Path, id: &str, last_active: &str) -> Result<()> {
        fs::create_dir_all(project_dir)?;

        let mut config = InstanceConfig::default();
        config.id = id.to_string();
        config.last_active = Some(last_active.to_string());

        write_instance_config(project_dir, &config)
    }

    #[test]
    fn update_global_sessions_adds_directory() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(config_path(project_dir.as_path()), "").unwrap();
        assert!(
            write_instance(
                &project_dir,
                "019bf290-cccc-7c23-ba1d-dce7e6d40693",
                "2026-02-07T05:00:00Z",
            )
            .is_ok()
        );

        let dirs = mgr.update_global_sessions(&project_dir).unwrap();

        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], project_dir.canonicalize().unwrap());
        assert!(mgr.index_path().exists());

        let session_path = mgr.index_path().join(format!(
            "019bf290-cccc-7c23-ba1d-dce7e6d40693{}",
            SESSION_TOML_SUFFIX
        ));
        assert!(session_path.exists());
    }

    #[test]
    fn update_global_sessions_removes_missing_vibebox_toml() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(config_path(project_dir.as_path()), "").unwrap();
        assert!(
            write_instance(
                &project_dir,
                "019bf290-cccc-7c23-ba1d-dce7e6d40693",
                "2026-02-07T05:00:00Z",
            )
            .is_ok()
        );
        let _ = mgr.update_global_sessions(&project_dir).unwrap();

        fs::remove_file(config_path(project_dir.as_path())).unwrap();
        let sessions = mgr.list_sessions().unwrap();
        assert!(sessions.is_empty());

        let session_path = mgr.index_path().join(format!(
            "019bf290-cccc-7c23-ba1d-dce7e6d40693{}",
            SESSION_TOML_SUFFIX
        ));
        assert!(!session_path.exists());
    }

    #[test]
    fn invalid_toml_returns_error() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        fs::create_dir_all(mgr.index_path()).unwrap();
        fs::write(
            mgr.index_path().join(format!("bad{SESSION_TOML_SUFFIX}")),
            "not toml",
        )
        .unwrap();

        let err = mgr.list_sessions().unwrap_err();
        assert!(matches!(err, SessionError::TomlDe(_)));
    }

    #[test]
    fn list_sessions_reads_session_files() {
        let temp = TempDir::new().unwrap();
        let mgr = manager(&temp);
        let project_dir = create_project_dir(&temp);
        fs::write(config_path(project_dir.as_path()), "").unwrap();
        assert!(
            write_instance(
                &project_dir,
                "019bf290-cccc-7c23-ba1d-dce7e6d40693",
                "2026-02-07T05:00:00Z",
            )
            .is_ok()
        );
        let _ = mgr.update_global_sessions(&project_dir).unwrap();

        let sessions = mgr.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "019bf290-cccc-7c23-ba1d-dce7e6d40693");
        assert_eq!(
            sessions[0].last_active.as_deref(),
            Some("2026-02-07T05:00:00Z")
        );
    }
}
