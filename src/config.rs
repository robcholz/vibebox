use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;

pub const CONFIG_FILENAME: &str = "vibebox.toml";

#[derive(Debug, Default, Deserialize)]
pub struct ProjectConfig {
    pub auto_shutdown_ms: Option<u64>,
}

pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(CONFIG_FILENAME)
}

pub fn ensure_config_file(project_root: &Path) -> Result<PathBuf, io::Error> {
    let path = config_path(project_root);
    if !path.exists() {
        fs::write(&path, "")?;
        tracing::info!(path = %path.display(), "created vibebox config");
    }
    Ok(path)
}

pub fn load_config(
    project_root: &Path,
) -> Result<ProjectConfig, Box<dyn std::error::Error + Send + Sync>> {
    let path = ensure_config_file(project_root)?;
    let raw = fs::read_to_string(&path)?;
    tracing::debug!(path = %path.display(), bytes = raw.len(), "loaded vibebox config");
    if raw.trim().is_empty() {
        return Ok(ProjectConfig::default());
    }
    Ok(toml::from_str::<ProjectConfig>(&raw)?)
}
