use std::{
    env,
    ffi::OsString,
    fs,
    path::Path,
    sync::Mutex,
};

use tempfile::TempDir;

use vibebox::{config, explain};
use vibebox::session_manager::INSTANCE_DIR_NAME;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = env::var_os(key);
        env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

#[test]
fn build_mount_rows_includes_defaults_and_custom_mounts() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let project = home.join("project");
    let cache_home = home.join("cache");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&cache_home).unwrap();

    let _home_guard = EnvGuard::set("HOME", &home);
    let _cache_guard = EnvGuard::set("XDG_CACHE_HOME", &cache_home);

    let mut box_cfg = config::BoxConfig::default();
    box_cfg.mounts = vec!["data:~/data:read-only".to_string()];
    let cfg = config::Config {
        box_cfg,
        supervisor: config::SupervisorConfig::default(),
    };

    let rows = explain::build_mount_rows(&project, &cfg).unwrap();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].host, "~/project");
    assert_eq!(rows[0].guest, "~/project");
    assert_eq!(rows[0].mode, "read-write");
    assert_eq!(rows[0].default_mount, "yes");

    assert_eq!(rows[1].host, "~/cache/vibebox/.guest-mise-cache");
    assert_eq!(rows[1].guest, "/root/.local/share/mise");
    assert_eq!(rows[1].mode, "read-write");
    assert_eq!(rows[1].default_mount, "yes");

    assert_eq!(rows[2].host, "~/project/data");
    assert_eq!(rows[2].guest, "~/data");
    assert_eq!(rows[2].mode, "read-only");
    assert_eq!(rows[2].default_mount, "no");
}

#[test]
fn build_network_rows_pending_without_instance_file() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let rows = explain::build_network_rows(&project).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].network_type, "NAT");
    assert_eq!(rows[0].vm_ip, "-");
    assert_eq!(rows[0].host_to_vm, "ssh: <pending>:22");
    assert_eq!(rows[0].vm_to_host, "none");
}

#[test]
fn build_network_rows_uses_instance_vm_ip() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let instance_dir = project.join(INSTANCE_DIR_NAME);
    fs::create_dir_all(&instance_dir).unwrap();
    fs::write(instance_dir.join("instance.toml"), "vm_ipv4 = \"10.1.2.3\"\n").unwrap();

    let rows = explain::build_network_rows(&project).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].network_type, "NAT");
    assert_eq!(rows[0].vm_ip, "10.1.2.3");
    assert_eq!(rows[0].host_to_vm, "ssh: 10.1.2.3:22");
    assert_eq!(rows[0].vm_to_host, "none");
}
