use std::{
    env, fs,
    io::{self},
    net::{SocketAddr, TcpStream},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    commands,
    session_manager::{INSTANCE_DIR_NAME, INSTANCE_FILENAME},
    vm::{self, LoginAction},
};

const SSH_KEY_NAME: &str = "ssh_key";
#[cfg_attr(feature = "mock-vm", allow(dead_code))]
pub(crate) const VM_ROOT_LOG_NAME: &str = "vm_root.log";
pub(crate) const STATUS_FILE_NAME: &str = "status.txt";
pub(crate) const DEFAULT_SSH_USER: &str = "vibecoder";
const SSH_CONNECT_RETRIES: usize = 30;
const SSH_CONNECT_DELAY_MS: u64 = 500;
const SSH_SETUP_SCRIPT: &str = include_str!("ssh.sh");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstanceConfig {
    #[serde(default)]
    id: String,
    #[serde(default = "default_ssh_user")]
    ssh_user: String,
    #[serde(default)]
    sudo_password: String,
    #[serde(default)]
    last_active: Option<String>,
    #[serde(default)]
    pub(crate) vm_ipv4: Option<String>,
}

impl InstanceConfig {
    pub(crate) fn ssh_user_display(&self) -> String {
        if self.ssh_user.trim().is_empty() {
            DEFAULT_SSH_USER.to_string()
        } else {
            self.ssh_user.clone()
        }
    }
}

fn default_ssh_user() -> String {
    DEFAULT_SSH_USER.to_string()
}

pub fn run_with_ssh(manager_conn: UnixStream) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = env::current_dir()?;
    tracing::info!(root = %project_root.display(), "starting ssh session");
    let instance_dir = ensure_instance_dir(&project_root)?;
    tracing::debug!(instance_dir = %instance_dir.display(), "instance dir ready");
    let (ssh_key, _ssh_pub) = ensure_ssh_keypair(&instance_dir)?;

    let config = load_or_create_instance_config(&instance_dir)?;
    let ssh_user = config.ssh_user.clone();
    tracing::debug!(ssh_user = %ssh_user, "loaded instance config");

    let _manager_conn = manager_conn;
    wait_for_vm_ipv4(&instance_dir, Duration::from_secs(480))?;

    let ip = load_or_create_instance_config(&instance_dir)?
        .vm_ipv4
        .ok_or("VM IPv4 not available")?;
    tracing::info!(ip = %ip, "vm ipv4 ready");

    run_ssh_session(ssh_key, ssh_user, ip)
}

pub fn ensure_instance_dir(project_root: &Path) -> Result<PathBuf, io::Error> {
    let instance_dir = project_root.join(INSTANCE_DIR_NAME);
    fs::create_dir_all(&instance_dir)?;
    Ok(instance_dir)
}

pub(crate) fn ensure_ssh_keypair(
    instance_dir: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let private_key = instance_dir.join(SSH_KEY_NAME);
    let public_key = instance_dir.join(format!("{SSH_KEY_NAME}.pub"));

    if private_key.exists() && public_key.exists() {
        return Ok((private_key, public_key));
    }

    if private_key.exists() {
        let _ = fs::remove_file(&private_key);
    }
    if public_key.exists() {
        let _ = fs::remove_file(&public_key);
    }

    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            private_key.to_str().ok_or("ssh key path not utf-8")?,
            "-C",
            "vibebox",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        return Err("ssh-keygen failed".into());
    }

    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&public_key, fs::Permissions::from_mode(0o644))?;

    Ok((private_key, public_key))
}

pub(crate) fn load_or_create_instance_config(
    instance_dir: &Path,
) -> Result<InstanceConfig, Box<dyn std::error::Error>> {
    let config_path = instance_dir.join(INSTANCE_FILENAME);
    let mut config = if config_path.exists() {
        let raw = fs::read_to_string(&config_path)?;
        toml::from_str::<InstanceConfig>(&raw)?
    } else {
        InstanceConfig {
            id: String::new(),
            ssh_user: default_ssh_user(),
            sudo_password: String::new(),
            last_active: None,
            vm_ipv4: None,
        }
    };

    let mut changed = false;
    if config.ssh_user.trim().is_empty() {
        config.ssh_user = default_ssh_user();
        changed = true;
    }

    if config.id.trim().is_empty() {
        config.id = Uuid::now_v7().to_string();
        changed = true;
    }

    if config.sudo_password.trim().is_empty() {
        config.sudo_password = generate_password();
        changed = true;
    }

    if !config_path.exists() || changed {
        write_instance_config(&config_path, &config)?;
    }

    Ok(config)
}

fn read_instance_config(
    instance_dir: &Path,
) -> Result<Option<InstanceConfig>, Box<dyn std::error::Error>> {
    let config_path = instance_dir.join(INSTANCE_FILENAME);
    if !config_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&config_path)?;
    let config = toml::from_str::<InstanceConfig>(&raw)?;
    Ok(Some(config))
}

pub fn read_instance_vm_ip(
    instance_dir: &Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let config = read_instance_config(instance_dir)?;
    Ok(config.and_then(|cfg| cfg.vm_ipv4))
}

pub fn read_instance_ssh_user(
    instance_dir: &Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let config = read_instance_config(instance_dir)?;
    Ok(config
        .map(|cfg| cfg.ssh_user)
        .filter(|user| !user.trim().is_empty()))
}

pub fn touch_last_active(instance_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_or_create_instance_config(instance_dir)?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    config.last_active = Some(now);
    write_instance_config(&instance_dir.join(INSTANCE_FILENAME), &config)?;
    Ok(())
}

pub(crate) fn write_instance_config(
    path: &Path,
    config: &InstanceConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = toml::to_string_pretty(config)?;
    fs::write(path, data)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn generate_password() -> String {
    Uuid::now_v7().simple().to_string()
}

#[cfg_attr(feature = "mock-vm", allow(dead_code))]
pub(crate) fn extract_ipv4(line: &str) -> Option<String> {
    let mut current = String::new();
    let mut best: Option<String> = None;

    for ch in line.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() || ch == '.' {
            current.push(ch);
        } else if !current.is_empty() {
            if is_ipv4_candidate(&current) {
                best = Some(current.clone());
                break;
            }
            current.clear();
        }
    }

    best
}

fn wait_for_vm_ipv4(
    instance_dir: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let mut next_log_at = start + Duration::from_secs(10);
    let mut next_status_check = start;
    tracing::info!("waiting for vm ipv4");
    let status_path = instance_dir.join(STATUS_FILE_NAME);
    let mut last_status: Option<String> = None;
    let mut status_missing = true;
    let mut once_hint = false;
    loop {
        let config = load_or_create_instance_config(instance_dir)?;
        if config.vm_ipv4.is_some() {
            let _ = fs::remove_file(&status_path);
            return Ok(());
        }
        if start.elapsed() > timeout {
            let _ = fs::remove_file(&status_path);
            return Err("Timed out waiting for VM IPv4".into());
        }
        let now = Instant::now();
        if now >= next_status_check {
            match fs::read_to_string(&status_path) {
                Ok(status) => {
                    status_missing = false;
                    let status = status.trim().to_string();
                    if !status.is_empty() && last_status.as_deref() != Some(status.as_str()) {
                        tracing::info!("[background]: {}", status);
                        last_status = Some(status);
                        next_log_at = now + Duration::from_secs(20);
                    }
                }
                Err(_) => {
                    status_missing = true;
                }
            }
            next_status_check = now + Duration::from_millis(500);
        }
        if now >= next_log_at {
            let waited = start.elapsed();
            if waited.as_secs() > 15 && !once_hint {
                tracing::info!(
                    "if vibebox is just initialized in this directory, it might take up to 1 minute depending on your machine, and then you can enjoy secure & speed vibecoding! go pack!"
                );
                once_hint = true;
            }
            if status_missing {
                tracing::info!("still waiting for vm ipv4, {}s elapsed", waited.as_secs(),);
            }
            next_log_at += Duration::from_secs(20);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn run_ssh_session(
    ssh_key: PathBuf,
    ssh_user: String,
    ip: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        if !ssh_port_open(&ip) {
            tracing::debug!(attempts, "ssh port doesn't open yet");
            tracing::info!(
                attempts,
                ip = %ip,
                "waiting for ssh port ({}/{})",
                attempts,
                SSH_CONNECT_RETRIES
            );
            if attempts >= SSH_CONNECT_RETRIES {
                return Err(
                    format!("ssh port not ready after {SSH_CONNECT_RETRIES} attempts").into(),
                );
            }
            thread::sleep(Duration::from_millis(SSH_CONNECT_DELAY_MS));
            continue;
        }

        tracing::info!(
            attempts,
            user = %ssh_user,
            ip = %ip,
            "starting ssh ({}/{})",
            attempts,
            SSH_CONNECT_RETRIES
        );
        let status = Command::new("ssh")
            .args([
                "-i",
                ssh_key.to_str().unwrap_or(".vibebox/ssh_key"),
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=5",
            ])
            .env_remove("LC_CTYPE")
            .env_remove("LC_ALL")
            .env_remove("LANG")
            .arg(format!("{ssh_user}@{ip}"))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();

        match status {
            Ok(status) if status.success() => {
                tracing::info!(status = %status, "ssh exited");
                break;
            }
            Ok(status) if status.code() == Some(255) => {
                tracing::warn!(status = %status, "ssh connection failed");
                if attempts >= SSH_CONNECT_RETRIES {
                    return Err(format!("ssh failed after {SSH_CONNECT_RETRIES} attempts").into());
                }
                thread::sleep(Duration::from_millis(500));
            }
            Ok(status) => {
                tracing::info!(status = %status, "ssh exited");
                break;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to start ssh");
                return Err(format!("failed to start ssh: {err}").into());
            }
        }
    }

    Ok(())
}

#[cfg_attr(feature = "mock-vm", allow(dead_code))]
fn is_ipv4_candidate(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    for part in parts {
        if part.is_empty() || part.len() > 3 {
            return false;
        }
        if part.parse::<u8>().is_err() {
            return false;
        }
    }
    true
}

fn ssh_port_open(ip: &str) -> bool {
    let addr: SocketAddr = match format!("{ip}:22").parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_ok()
}

pub(crate) fn build_ssh_login_actions(
    config: &Arc<Mutex<InstanceConfig>>,
    project_name: &str,
    project_guest_dir: &str,
    guest_dir: &str,
    key_name: &str,
    home_links_script: &str,
) -> Vec<LoginAction> {
    let config_guard = config.lock().expect("config mutex poisoned");
    let ssh_user = config_guard.ssh_user.clone();
    let sudo_password = config_guard.sudo_password.clone();
    drop(config_guard);

    let key_path = format!("{guest_dir}/{key_name}.pub");

    let setup_script = SSH_SETUP_SCRIPT
        .replace("__SSH_USER__", &ssh_user)
        .replace("__SUDO_PASSWORD__", &sudo_password)
        .replace("__PROJECT_NAME__", project_name)
        .replace("__PROJECT_GUEST_DIR__", project_guest_dir)
        .replace("__KEY_PATH__", &key_path)
        .replace("__VIBEBOX_SHELL_SCRIPT__", &commands::render_shell_script())
        .replace("__VIBEBOX_HOME_LINKS__", home_links_script);
    let setup = vm::script_command_from_content("ssh_setup", &setup_script)
        .expect("ssh setup script contained invalid marker");

    vec![LoginAction::Send(setup)]
}
