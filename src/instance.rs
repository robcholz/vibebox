use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, IsTerminal, Read},
    net::{SocketAddr, TcpStream},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    commands,
    session_manager::INSTANCE_DIR_NAME,
    vm::{self, LoginAction},
};

const SSH_KEY_NAME: &str = "ssh_key";
const INSTANCE_FILENAME: &str = "instance.toml";
const DEFAULT_SSH_USER: &str = "vibecoder";
const SSH_CONNECT_RETRIES: usize = 10;
const SSH_CONNECT_DELAY_MS: u64 = 500;
const SSH_SETUP_SCRIPT: &str = include_str!("ssh.sh");
const STATUS_PREFIX: &str = "status:";
const STATUS_ERROR_PREFIX: &str = "error:";

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("unexpected disconnection from vm manager")]
    UnexpectedDisconnection,
}

fn default_ssh_user() -> String {
    DEFAULT_SSH_USER.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_ssh_user")]
    pub ssh_user: String,
    #[serde(default)]
    sudo_password: String,
    #[serde(default)]
    pub last_active: Option<String>,
    #[serde(default)]
    pub vm_ipv4: Option<String>,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            ssh_user: DEFAULT_SSH_USER.to_string(),
            sudo_password: Uuid::now_v7().simple().to_string(),
            last_active: None,
            vm_ipv4: None,
        }
    }
}

pub fn run_with_ssh(manager_conn: UnixStream) -> Result<()> {
    let project_root = env::current_dir()?;
    tracing::info!(root = %project_root.display(), "starting ssh session");
    let instance_dir = ensure_instance_dir(&project_root)?;
    tracing::debug!(instance_dir = %instance_dir.display(), "instance dir ready");
    let (ssh_key, _ssh_pub) = ensure_ssh_keypair(&instance_dir)?;

    let config = load_or_create_instance_config(&project_root)?;
    let ssh_user = config.ssh_user.clone();
    tracing::debug!(ssh_user = %ssh_user, "loaded instance config");

    wait_for_vm_ipv4(&project_root, Duration::from_secs(480), &manager_conn)?;

    let ip = load_or_create_instance_config(&project_root)?
        .vm_ipv4
        .with_context(|| "failed to load instance IP address")?;
    tracing::info!(ip = %ip, "vm ipv4 ready");

    run_ssh_session(ssh_key, ssh_user, ip, manager_conn)
}

pub fn ensure_instance_dir(project_root: &Path) -> Result<PathBuf, io::Error> {
    let instance_dir = project_root.join(INSTANCE_DIR_NAME);
    fs::create_dir_all(&instance_dir)?;
    Ok(instance_dir)
}

pub fn ensure_ssh_keypair(instance_dir: &Path) -> Result<(PathBuf, PathBuf)> {
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
            private_key
                .to_str()
                .with_context(|| "ssh key path not utf-8")?,
            "-C",
            "vibebox",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        bail!("ssh-keygen failed");
    }

    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&public_key, fs::Permissions::from_mode(0o644))?;

    Ok((private_key, public_key))
}

pub fn load_or_create_instance_config(project_dir: &Path) -> Result<InstanceConfig> {
    let mut exist = true;
    let mut config = read_instance_config(project_dir).unwrap_or_else(|_| {
        exist = false;
        InstanceConfig::default()
    });

    let mut changed = false;
    if config.ssh_user.trim().is_empty() {
        config.ssh_user = InstanceConfig::default().ssh_user;
        changed = true;
    }

    if config.id.trim().is_empty() {
        config.id = InstanceConfig::default().id;
        changed = true;
    }

    if config.sudo_password.trim().is_empty() {
        config.sudo_password = InstanceConfig::default().sudo_password;
        changed = true;
    }

    if !exist || changed {
        write_instance_config(project_dir, &config)?;
    }

    Ok(config)
}

pub fn read_instance_config(project_dir: &Path) -> Result<InstanceConfig> {
    // todo maybe verify schema?
    let config_path = project_dir.join(INSTANCE_DIR_NAME).join(INSTANCE_FILENAME);
    if !config_path.exists() {
        bail!("instance config file does not exist");
    }
    let raw = fs::read_to_string(&config_path)?;
    let config = toml::from_str::<InstanceConfig>(&raw)?;
    Ok(config)
}

pub fn touch_last_active(project_dir: &Path) -> Result<()> {
    let mut config = load_or_create_instance_config(project_dir)?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    config.last_active = Some(now);
    write_instance_config(project_dir, &config)?;
    Ok(())
}

pub fn write_instance_config(project_dir: &Path, config: &InstanceConfig) -> Result<()> {
    let path = project_dir.join(INSTANCE_DIR_NAME).join(INSTANCE_FILENAME);
    let data = toml::to_string_pretty(config)?;
    fs::create_dir_all(project_dir.join(INSTANCE_DIR_NAME))?;
    fs::write(&path, data)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg_attr(feature = "mock-vm", allow(dead_code))]
pub fn extract_ipv4(line: &str) -> Option<String> {
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

fn handle_manager_line(line: &str, last_status: &mut Option<String>) -> Result<()> {
    if let Some(status) = line.strip_prefix(STATUS_PREFIX) {
        let status = status.trim();
        if let Some(message) = status.strip_prefix(STATUS_ERROR_PREFIX) {
            let message = message.trim();
            if message.is_empty() {
                bail!("vm manager reported startup failure");
            }
            bail!(message.to_string());
        }
        if !status.is_empty() && last_status.as_deref() != Some(status) {
            tracing::info!("[background]: {}", status);
            *last_status = Some(status.to_string());
        }
    }
    Ok(())
}

fn wait_for_vm_ipv4(
    project_dir: &Path,
    timeout: Duration,
    manager_conn: &UnixStream,
) -> Result<()> {
    let start = Instant::now();
    let mut next_log_at = start + Duration::from_secs(10);
    let mut stream = manager_conn.try_clone()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut read_buf = [0u8; 1024];
    let mut pending = String::new();
    tracing::info!("waiting for vm ipv4");
    let mut last_status: Option<String> = None;
    let mut once_hint = false;
    loop {
        match stream.read(&mut read_buf) {
            Ok(0) => {
                bail!("vm manager disconnected before VM became ready");
            }
            Ok(n) => {
                pending.push_str(&String::from_utf8_lossy(&read_buf[..n]));
                while let Some(pos) = pending.find('\n') {
                    let line = pending[..pos].trim().to_string();
                    pending.drain(..=pos);
                    handle_manager_line(&line, &mut last_status)?;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                tracing::warn!(error = %err, "failed to read vm manager status stream");
            }
        }

        let config = load_or_create_instance_config(project_dir)?;
        if config.vm_ipv4.is_some() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            bail!("timed out waiting for VM IPv4");
        }

        let now = Instant::now();
        if now >= next_log_at {
            let waited = start.elapsed();
            if waited.as_secs() > 15 && !once_hint {
                tracing::info!(
                    "if vibebox is just initialized in this directory, it might take up to 1 minute depending on your machine, and then you can enjoy secure & speed vibecoding! go pack!"
                );
                once_hint = true;
            }
            tracing::info!("still waiting for vm ipv4, {}s elapsed", waited.as_secs(),);
            next_log_at += Duration::from_secs(20);
        }
    }
}

fn run_ssh_session(
    ssh_key: PathBuf,
    ssh_user: String,
    ip: String,
    manager_conn: UnixStream,
) -> Result<()> {
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
                bail!("ssh port not ready after {SSH_CONNECT_RETRIES} attempts");
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
        let child = Command::new("ssh")
            .args([
                "-i",
                ssh_key.to_str().with_context(|| "invalid path")?,
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
            .spawn();

        match child {
            Ok(mut child) => {
                let done = Arc::new(AtomicBool::new(false));
                let done_for_monitor = done.clone();
                let (disconnect_tx, disconnect_rx) = mpsc::channel::<()>();
                let mut manager_stream = manager_conn.try_clone()?;
                let _ = manager_stream.set_read_timeout(Some(Duration::from_millis(250)));
                thread::spawn(move || {
                    let mut buf = [0u8; 1];
                    while !done_for_monitor.load(Ordering::Relaxed) {
                        match manager_stream.read(&mut buf) {
                            Ok(0) => {
                                let _ = disconnect_tx.send(());
                                return;
                            }
                            Ok(_) => {}
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
                            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => {
                                let _ = disconnect_tx.send(());
                                return;
                            }
                        }
                    }
                });

                let status = loop {
                    if disconnect_rx.try_recv().is_ok() {
                        done.store(true, Ordering::Relaxed);
                        terminate_ssh_child(&mut child);
                        restore_terminal_after_disconnect();
                        return Err(InstanceError::UnexpectedDisconnection.into());
                    }
                    if let Some(status) = child.try_wait()? {
                        done.store(true, Ordering::Relaxed);
                        break status;
                    }
                    thread::sleep(Duration::from_millis(100));
                };

                if status.success() {
                    tracing::info!(status = %status, "ssh exited");
                    break;
                }
                if status.code() == Some(255) {
                    tracing::warn!(status = %status, "ssh connection failed");
                    if attempts >= SSH_CONNECT_RETRIES {
                        bail!("ssh failed after {SSH_CONNECT_RETRIES} attempts");
                    }
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                tracing::info!(status = %status, "ssh exited");
                break;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to start ssh");
                bail!("failed to start ssh: {err}");
            }
        }
    }

    Ok(())
}

fn terminate_ssh_child(child: &mut Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_millis(700);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn restore_terminal_after_disconnect() {
    if io::stdin().is_terminal() {
        let _ = Command::new("stty").arg("sane").status();
    }
    eprintln!();
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
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

pub fn build_ssh_login_actions(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_manager_line_updates_status() {
        let mut last_status = None;
        handle_manager_line("status: preparing VM image...", &mut last_status)
            .expect("status should be accepted");
        assert_eq!(last_status.as_deref(), Some("preparing VM image..."));
    }

    #[test]
    fn handle_manager_line_ignores_non_status_lines() {
        let mut last_status = None;
        handle_manager_line("pid=123", &mut last_status).expect("non-status lines are ignored");
        assert!(last_status.is_none());
    }

    #[test]
    fn handle_manager_line_surfaces_error_status() {
        let mut last_status = None;
        let err = handle_manager_line("status: error: vm failed to boot", &mut last_status)
            .expect_err("error status should fail");
        assert_eq!(err.to_string(), "vm failed to boot");
    }
}
