use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::vm::DirectoryShare;

pub const CONFIG_FILENAME: &str = "vibebox.toml";

const DEFAULT_CPU_COUNT: usize = 2;
const DEFAULT_RAM_MB: u64 = 2048;
const DEFAULT_AUTO_SHUTDOWN_MS: u64 = 20000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "box")]
    pub box_cfg: BoxConfig,
    pub supervisor: SupervisorConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            box_cfg: BoxConfig::default(),
            supervisor: SupervisorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxConfig {
    pub cpu_count: usize,
    pub ram_mb: u64,
    pub mounts: Vec<String>,
}

impl Default for BoxConfig {
    fn default() -> Self {
        Self {
            cpu_count: default_cpu_count(),
            ram_mb: default_ram_mb(),
            mounts: default_mounts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    pub auto_shutdown_ms: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            auto_shutdown_ms: default_auto_shutdown_ms(),
        }
    }
}

fn default_cpu_count() -> usize {
    DEFAULT_CPU_COUNT
}

fn default_ram_mb() -> u64 {
    DEFAULT_RAM_MB
}

fn default_auto_shutdown_ms() -> u64 {
    DEFAULT_AUTO_SHUTDOWN_MS
}

fn default_mounts() -> Vec<String> {
    let home = match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home),
        Err(_) => return Vec::new(),
    };

    let mut mounts = Vec::new();
    let codex_host = home.join(".codex");
    if codex_host.exists() {
        mounts.push("~/.codex:/usr/local/codex:read-write".to_string());
    }
    let claude_host = home.join(".claude");
    if claude_host.exists() {
        mounts.push("~/.claude:/usr/local/claude:read-write".to_string());
    }
    mounts
}

pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(CONFIG_FILENAME)
}

pub fn ensure_config_file(project_root: &Path) -> Result<PathBuf, io::Error> {
    let path = config_path(project_root);
    if !path.exists() {
        let default_config = Config::default();
        let contents = toml::to_string_pretty(&default_config).unwrap_or_default();
        fs::write(&path, contents)?;
        tracing::info!(path = %path.display(), "created vibebox config");
    }
    Ok(path)
}

pub fn load_config(project_root: &Path) -> Config {
    let path = match ensure_config_file(project_root) {
        Ok(path) => path,
        Err(err) => die(&format!("failed to create config: {err}")),
    };
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => die(&format!("failed to read config: {err}")),
    };
    let trimmed = raw.trim();
    tracing::debug!(path = %path.display(), bytes = raw.len(), "loaded vibebox config");
    if trimmed.is_empty() {
        die(&format!(
            "config file ({}) is empty. Required fields: [box].cpu_count (integer), [box].ram_mb (integer), [box].mounts (array of strings), [supervisor].auto_shutdown_ms (integer)",
            path.display()
        ));
    }

    let value: toml::Value = match toml::from_str(trimmed) {
        Ok(value) => value,
        Err(err) => die(&format!("invalid config: {err}")),
    };
    let schema_errors = validate_schema(&value);
    if !schema_errors.is_empty() {
        let message = format!(
            "config file ({}) is missing or invalid fields:\n- {}",
            path.display(),
            schema_errors.join("\n- ")
        );
        die(&message);
    }

    let config: Config = match toml::from_str(trimmed) {
        Ok(config) => config,
        Err(err) => die(&format!("invalid config: {err}")),
    };
    validate_or_exit(&config);
    config
}

fn validate_schema(value: &toml::Value) -> Vec<String> {
    let mut errors = Vec::new();
    let root = match value.as_table() {
        Some(table) => table,
        None => {
            errors.push("config must be a table".to_string());
            return errors;
        }
    };

    match root.get("box") {
        None => errors.push("missing [box] table".to_string()),
        Some(value) => match value.as_table() {
            Some(table) => {
                validate_int(table, "cpu_count", "[box].cpu_count (integer)", &mut errors);
                validate_int(table, "ram_mb", "[box].ram_mb (integer)", &mut errors);
                validate_string_array(
                    table,
                    "mounts",
                    "[box].mounts (array of strings)",
                    &mut errors,
                );
            }
            None => errors.push("[box] must be a table".to_string()),
        },
    }

    match root.get("supervisor") {
        None => errors.push("missing [supervisor] table".to_string()),
        Some(value) => match value.as_table() {
            Some(table) => {
                validate_int(
                    table,
                    "auto_shutdown_ms",
                    "[supervisor].auto_shutdown_ms (integer)",
                    &mut errors,
                );
            }
            None => errors.push("[supervisor] must be a table".to_string()),
        },
    }

    errors
}

fn validate_int(table: &toml::value::Table, key: &str, label: &str, errors: &mut Vec<String>) {
    match table.get(key) {
        None => errors.push(format!("missing {label}")),
        Some(value) => {
            if value.as_integer().is_none() {
                errors.push(format!("invalid {label}: expected integer"));
            }
        }
    }
}

fn validate_string_array(
    table: &toml::value::Table,
    key: &str,
    label: &str,
    errors: &mut Vec<String>,
) {
    match table.get(key) {
        None => errors.push(format!("missing {label}")),
        Some(value) => match value.as_array() {
            Some(values) => {
                if values.iter().any(|value| !value.is_str()) {
                    errors.push(format!("invalid {label}: expected array of strings"));
                }
            }
            None => errors.push(format!("invalid {label}: expected array of strings")),
        },
    }
}

fn validate_or_exit(config: &Config) {
    if config.box_cfg.cpu_count == 0 {
        die("box.cpu_count must be >= 1");
    }
    if config.box_cfg.ram_mb == 0 {
        die("box.ram_mb must be >= 1");
    }
    if config.supervisor.auto_shutdown_ms == 0 {
        die("supervisor.auto_shutdown_ms must be >= 1");
    }
    for spec in &config.box_cfg.mounts {
        if let Err(err) = DirectoryShare::from_mount_spec(spec) {
            die(&format!("invalid mount spec '{spec}': {err}"));
        }
    }
}

fn die(message: &str) -> ! {
    eprintln!("[vibebox] {message}");
    std::process::exit(1);
}
