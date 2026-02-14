use anyhow::{Context, Error, Result, bail};
use bytesize::ByteSize;
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::vm::DirectoryShare;

const CONFIG_FILENAME: &str = "vibebox.toml";
pub const CONFIG_PATH_ENV: &str = "VIBEBOX_CONFIG_PATH";

const DEFAULT_CPU_COUNT: usize = 2;
const DEFAULT_RAM_MB: u64 = 2048;
const DEFAULT_AUTO_SHUTDOWN_MS: u64 = 20000;
const DEFAULT_DISK_GB: u64 = 5;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "box")]
    pub box_cfg: BoxConfig,
    pub supervisor: SupervisorConfig,
}

const MI_B: u64 = 1024 * 1024;
const GI_B: u64 = 1024 * 1024 * 1024;

mod serde_mb {
    use super::{ByteSize, MI_B};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(v: &ByteSize, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = v.0;
        if !bytes.is_multiple_of(MI_B) {
            return Err(serde::ser::Error::custom(
                "ram_mb must be an integer number of MB",
            ));
        }
        s.serialize_u64(bytes / MI_B)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<ByteSize, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mb = u64::deserialize(d)?;
        let bytes = mb
            .checked_mul(MI_B)
            .ok_or_else(|| serde::de::Error::custom("ram_mb overflow"))?;
        Ok(ByteSize(bytes))
    }
}

mod serde_gb {
    use super::{ByteSize, GI_B};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(v: &ByteSize, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = v.0;
        if !bytes.is_multiple_of(GI_B) {
            return Err(serde::ser::Error::custom(
                "disk_gb must be an integer number of GB",
            ));
        }
        s.serialize_u64(bytes / GI_B)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<ByteSize, D::Error>
    where
        D: Deserializer<'de>,
    {
        let gb = u64::deserialize(d)?;
        let bytes = gb
            .checked_mul(GI_B)
            .ok_or_else(|| serde::de::Error::custom("disk_gb overflow"))?;
        Ok(ByteSize(bytes))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxConfig {
    pub cpu_count: usize,
    #[serde(rename = "ram_mb", with = "serde_mb")]
    pub ram_size: ByteSize,
    #[serde(rename = "disk_gb", with = "serde_gb")]
    pub disk_size: ByteSize,
    pub mounts: Vec<String>,
}

impl Default for BoxConfig {
    fn default() -> Self {
        Self {
            cpu_count: DEFAULT_CPU_COUNT,
            ram_size: ByteSize::mib(DEFAULT_RAM_MB),
            disk_size: ByteSize::gib(DEFAULT_DISK_GB),
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
            auto_shutdown_ms: DEFAULT_AUTO_SHUTDOWN_MS,
        }
    }
}

fn default_mounts() -> Vec<String> {
    vec![
        "~/.codex:~/.codex:read-write".into(),
        "~/.claude:~/.claude:read-write".into(),
    ]
}

pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(CONFIG_FILENAME)
}

pub fn ensure_config_file(project_root: &Path, override_path: Option<&Path>) -> Result<PathBuf> {
    let path = resolve_config_path(project_root, override_path)?;
    if !path.exists() {
        let default_config = Config::default();
        let contents = toml::to_string_pretty(&default_config).unwrap_or_default();
        fs::write(&path, contents)?;
        tracing::info!(path = %path.display(), "created vibebox config");
    }
    Ok(path)
}

pub fn load_config(project_root: &Path) -> Result<Config> {
    load_config_with_path(project_root, None)
}

pub fn load_config_with_path(project_root: &Path, override_path: Option<&Path>) -> Result<Config> {
    let path =
        ensure_config_file(project_root, override_path).context("failed to create config")?;
    let raw = fs::read_to_string(&path).context("failed to read config")?;
    let trimmed = raw.trim();
    tracing::debug!(path = %path.display(), bytes = raw.len(), "loaded vibebox config");
    if trimmed.is_empty() {
        bail!(format!(
            "config file ({}) is empty. Required fields: [box].cpu_count (integer), [box].ram_mb (integer), [box].disk_gb (integer), [box].mounts (array of strings), [supervisor].auto_shutdown_ms (integer)",
            path.display()
        ));
    }

    let value: toml::Value = toml::from_str(trimmed).context("invalid config")?;
    let schema_errors = validate_schema(&value);
    if !schema_errors.is_empty() {
        let message = format!(
            "config file ({}) is missing or invalid fields:\n- {}",
            path.display(),
            schema_errors.join("\n- ")
        );
        bail!(message);
    }

    let config: Config = toml::from_str(trimmed).context("invalid config")?;
    validate_config(&config).map_err(Error::msg)?;
    Ok(config)
}

fn resolve_config_path(project_root: &Path, override_path: Option<&Path>) -> Result<PathBuf> {
    let env_override = env::var_os(CONFIG_PATH_ENV).map(PathBuf::from);
    resolve_config_path_inner(project_root, override_path, env_override)
}

fn resolve_config_path_inner(
    project_root: &Path,
    override_path: Option<&Path>,
    env_override: Option<PathBuf>,
) -> Result<PathBuf> {
    let root = fs::canonicalize(project_root).context("failed to resolve project root")?;

    let selected_path = override_path.map(PathBuf::from).or(env_override);
    let raw_path = if let Some(path) = selected_path {
        if path.is_absolute() {
            path
        } else {
            project_root.join(path)
        }
    } else {
        config_path(project_root)
    };

    let normalized = normalize_path(&raw_path);
    let resolved =
        resolve_path_for_boundary_check(&normalized).context("failed to resolve config path")?;
    if !resolved.starts_with(&root) {
        bail!(
            "config path must be within {}: {}",
            root.display(),
            resolved.display()
        );
    }
    Ok(normalized)
}

fn resolve_path_for_boundary_check(path: &Path) -> Result<PathBuf, io::Error> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let (ancestor, missing) = nearest_existing_ancestor(path)?;
    let mut resolved = fs::canonicalize(ancestor)?;
    for part in missing {
        resolved.push(part);
    }
    Ok(resolved)
}

fn nearest_existing_ancestor(path: &Path) -> Result<(&Path, Vec<std::ffi::OsString>), io::Error> {
    let mut current = path;
    let mut missing = Vec::new();
    while !current.exists() {
        let name = current.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path has no existing ancestor: {}", path.display()),
            )
        })?;
        missing.push(name.to_os_string());
        current = current.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path has no parent: {}", path.display()),
            )
        })?;
    }
    missing.reverse();
    Ok((current, missing))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => {
                normalized.push(std::path::MAIN_SEPARATOR.to_string());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = normalized.pop();
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
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
                validate_int(table, "disk_gb", "[box].disk_gb (integer)", &mut errors);
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

fn validate_config(config: &Config) -> Result<(), String> {
    if config.box_cfg.cpu_count == 0 {
        return Err("box.cpu_count must be >= 1".to_string());
    }
    if config.box_cfg.ram_size.as_mib() == 0.0 {
        return Err("box.ram_mb must be >= 1".to_string());
    }
    if config.box_cfg.disk_size.as_gib() == 0.0 {
        return Err("box.disk_gb must be >= 1".to_string());
    }
    if config.supervisor.auto_shutdown_ms == 0 {
        return Err("supervisor.auto_shutdown_ms must be >= 1".to_string());
    }
    for spec in &config.box_cfg.mounts {
        if let Err(err) = DirectoryShare::from_mount_spec(spec) {
            return Err(format!("invalid mount spec '{spec}': {err}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe {
                    env::set_var(self.key, value);
                },
                None => unsafe {
                    env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    fn default_config_serializes_with_legacy_keys() {
        let cfg = Config::default();
        let serialized = toml::to_string(&cfg).expect("default config should serialize");

        assert!(serialized.contains("ram_mb = 2048"));
        assert!(serialized.contains("disk_gb = 5"));
        assert!(!serialized.contains("ram_size"));
        assert!(!serialized.contains("disk_size"));
    }

    #[test]
    fn config_deserializes_sizes_from_mb_and_gb() {
        let raw = r#"
[box]
cpu_count = 4
ram_mb = 3072
disk_gb = 12
mounts = ["~/src:~/src:read-write"]

[supervisor]
auto_shutdown_ms = 15000
"#;
        let cfg: Config = toml::from_str(raw).expect("config should deserialize");

        assert_eq!(cfg.box_cfg.cpu_count, 4);
        assert_eq!(cfg.box_cfg.ram_size.as_u64(), ByteSize::mib(3072).as_u64());
        assert_eq!(cfg.box_cfg.disk_size.as_u64(), ByteSize::gib(12).as_u64());
        assert_eq!(cfg.supervisor.auto_shutdown_ms, 15000);
    }

    #[test]
    fn serialize_rejects_non_integral_mb_or_gb() {
        let cfg = Config {
            box_cfg: BoxConfig {
                cpu_count: 2,
                ram_size: ByteSize::b((2 * MI_B) + 1),
                disk_size: ByteSize::gib(5),
                mounts: default_mounts(),
            },
            supervisor: SupervisorConfig::default(),
        };

        let err = toml::to_string(&cfg).expect_err("serialization should reject invalid MB");
        assert!(
            err.to_string()
                .contains("ram_mb must be an integer number of MB")
        );

        let cfg = Config {
            box_cfg: BoxConfig {
                cpu_count: 2,
                ram_size: ByteSize::mib(2048),
                disk_size: ByteSize::b((5 * GI_B) + 1),
                mounts: default_mounts(),
            },
            supervisor: SupervisorConfig::default(),
        };

        let err = toml::to_string(&cfg).expect_err("serialization should reject invalid GB");
        assert!(
            err.to_string()
                .contains("disk_gb must be an integer number of GB")
        );
    }

    #[test]
    fn normalize_path_removes_dot_and_parent_components() {
        let normalized = normalize_path(Path::new("/tmp/project/./nested/../config.toml"));
        assert_eq!(normalized, PathBuf::from("/tmp/project/config.toml"));
    }

    #[test]
    fn validate_schema_returns_errors_for_missing_required_fields() {
        let value: toml::Value = toml::from_str(
            r#"
[box]
cpu_count = 2
"#,
        )
        .expect("toml should parse");

        let errors = validate_schema(&value);

        assert!(errors.iter().any(|e| e == "missing [supervisor] table"));
        assert!(errors.iter().any(|e| e == "missing [box].ram_mb (integer)"));
        assert!(
            errors
                .iter()
                .any(|e| e == "missing [box].disk_gb (integer)")
        );
        assert!(
            errors
                .iter()
                .any(|e| e == "missing [box].mounts (array of strings)")
        );
    }

    #[test]
    fn validate_schema_errors_when_supervisor_is_not_table() {
        let value: toml::Value = toml::from_str(
            r#"
supervisor = 123

[box]
cpu_count = 2
ram_mb = 2048
disk_gb = 5
mounts = []
"#,
        )
        .expect("toml should parse");

        let errors = validate_schema(&value);
        assert!(errors.iter().any(|e| e == "[supervisor] must be a table"));
    }

    #[test]
    fn ensure_config_file_creates_default_config_if_absent() {
        let temp = TempDir::new().expect("temp dir should be created");
        let root = fs::canonicalize(temp.path()).expect("temp dir should canonicalize");

        let path = ensure_config_file(&root, None).expect("config should be created");
        let raw = fs::read_to_string(&path).expect("created config should be readable");
        let parsed: Config = toml::from_str(&raw).expect("created config should be valid");

        assert_eq!(path, root.join("vibebox.toml"));
        assert_eq!(parsed.box_cfg.cpu_count, DEFAULT_CPU_COUNT);
        assert_eq!(
            parsed.box_cfg.ram_size.as_u64(),
            ByteSize::mib(DEFAULT_RAM_MB).as_u64()
        );
        assert_eq!(
            parsed.box_cfg.disk_size.as_u64(),
            ByteSize::gib(DEFAULT_DISK_GB).as_u64()
        );
    }

    #[test]
    fn load_config_creates_and_loads_default_config() {
        let _lock = ENV_MUTEX.lock().expect("env lock should be acquired");
        let temp = TempDir::new().expect("temp dir should be created");
        let root = fs::canonicalize(temp.path()).expect("temp dir should canonicalize");
        let home = root.join("home");
        fs::create_dir_all(home.join(".codex")).expect("home .codex should be created");
        fs::create_dir_all(home.join(".claude")).expect("home .claude should be created");
        let _home_guard = EnvGuard::set("HOME", &home);

        let cfg = load_config(&root).expect("load_config should succeed");

        assert_eq!(cfg.box_cfg.cpu_count, DEFAULT_CPU_COUNT);
        assert_eq!(
            cfg.box_cfg.ram_size.as_u64(),
            ByteSize::mib(DEFAULT_RAM_MB).as_u64()
        );
        assert_eq!(
            cfg.box_cfg.disk_size.as_u64(),
            ByteSize::gib(DEFAULT_DISK_GB).as_u64()
        );
        assert!(root.join("vibebox.toml").exists());
    }

    #[test]
    fn load_config_with_path_uses_override_path() {
        let _lock = ENV_MUTEX.lock().expect("env lock should be acquired");
        let temp = TempDir::new().expect("temp dir should be created");
        let root = fs::canonicalize(temp.path()).expect("temp dir should canonicalize");
        let home = root.join("home");
        fs::create_dir_all(home.join(".codex")).expect("home .codex should be created");
        fs::create_dir_all(home.join(".claude")).expect("home .claude should be created");
        let _home_guard = EnvGuard::set("HOME", &home);
        let override_path = root.join("custom.toml");

        fs::write(
            &override_path,
            r#"
[box]
cpu_count = 6
ram_mb = 4096
disk_gb = 9
mounts = ["~/.codex:~/.codex:read-write", "~/.claude:~/.claude:read-write"]

[supervisor]
auto_shutdown_ms = 12345
"#,
        )
        .expect("override config should be written");

        let cfg = load_config_with_path(&root, Some(Path::new("custom.toml")))
            .expect("load_config_with_path should succeed");

        assert_eq!(cfg.box_cfg.cpu_count, 6);
        assert_eq!(cfg.box_cfg.ram_size.as_u64(), ByteSize::mib(4096).as_u64());
        assert_eq!(cfg.box_cfg.disk_size.as_u64(), ByteSize::gib(9).as_u64());
        assert_eq!(cfg.supervisor.auto_shutdown_ms, 12345);
        assert!(!root.join("vibebox.toml").exists());
    }

    #[test]
    fn resolve_config_path_uses_env_override_when_cli_override_missing() {
        let temp = TempDir::new().expect("temp dir should be created");
        let root = fs::canonicalize(temp.path()).expect("temp dir should canonicalize");

        let resolved = resolve_config_path_inner(&root, None, Some(PathBuf::from("custom.toml")))
            .expect("env override path should resolve");

        assert_eq!(resolved, root.join("custom.toml"));
    }

    #[test]
    fn resolve_config_path_rejects_env_override_outside_project() {
        let temp = TempDir::new().expect("temp dir should be created");
        let root = fs::canonicalize(temp.path()).expect("temp dir should canonicalize");

        let err = resolve_config_path_inner(&root, None, Some(PathBuf::from("../escape.toml")))
            .expect_err("outside-project path should be rejected");

        assert!(
            err.to_string().contains("config path must be within"),
            "expected bounds-check error, got: {err}"
        );
    }

    #[test]
    fn validate_config_rejects_invalid_values() {
        let cfg = Config {
            box_cfg: BoxConfig {
                cpu_count: 0,
                ram_size: ByteSize::mib(2048),
                disk_size: ByteSize::gib(5),
                mounts: vec![],
            },
            supervisor: SupervisorConfig::default(),
        };
        let err = validate_config(&cfg).expect_err("cpu_count=0 should fail");
        assert_eq!(err, "box.cpu_count must be >= 1");

        let cfg = Config {
            box_cfg: BoxConfig {
                cpu_count: 2,
                ram_size: ByteSize::mib(2048),
                disk_size: ByteSize::gib(5),
                mounts: vec!["/definitely/missing:/tmp/missing:read-write".to_string()],
            },
            supervisor: SupervisorConfig::default(),
        };
        let err = validate_config(&cfg).expect_err("invalid mount should fail");
        assert!(err.starts_with("invalid mount spec"));
    }

    #[test]
    fn resolve_config_path_accepts_symlinked_project_root() {
        let temp = TempDir::new().expect("temp dir should be created");
        let actual_root = temp.path().join("actual");
        let link_root = temp.path().join("linked");
        fs::create_dir_all(&actual_root).expect("actual root should exist");
        std::os::unix::fs::symlink(&actual_root, &link_root).expect("symlink should be created");

        let resolved = resolve_config_path_inner(&link_root, Some(Path::new("vibebox.toml")), None)
            .expect("symlinked project root should resolve");

        assert_eq!(resolved, link_root.join("vibebox.toml"));
    }

    #[test]
    fn resolve_config_path_rejects_symlink_escape() {
        let temp = TempDir::new().expect("temp dir should be created");
        let project_root = temp.path().join("project");
        let outside_root = temp.path().join("outside");
        fs::create_dir_all(&project_root).expect("project root should exist");
        fs::create_dir_all(&outside_root).expect("outside root should exist");
        std::os::unix::fs::symlink(&outside_root, project_root.join("link"))
            .expect("escape symlink should be created");

        let err = resolve_config_path_inner(&project_root, Some(Path::new("link/cfg.toml")), None)
            .expect_err("symlink escape should be rejected");
        assert!(
            err.to_string().contains("config path must be within"),
            "expected bounds-check error, got: {err}"
        );
    }
}
