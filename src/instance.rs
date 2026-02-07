use crate::session_manager::INSTANCE_TOML_FILENAME;
use std::{
    env, fs,
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    os::unix::{fs::PermissionsExt, io::OwnedFd, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use std::sync::mpsc::Sender;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    session_manager::INSTANCE_DIR_NAME,
    tui::{self, AppState},
    vm::{self, LoginAction, VmInput},
};

const SSH_KEY_NAME: &str = "ssh_key";
#[allow(dead_code)]
const SERIAL_LOG_NAME: &str = "serial.log";
const DEFAULT_SSH_USER: &str = "vibecoder";
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
    tracing::debug!("waiting for vm ipv4");
    wait_for_vm_ipv4(&instance_dir, Duration::from_secs(120))?;

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
        .stdout(Stdio::null())
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
    let config_path = instance_dir.join(INSTANCE_TOML_FILENAME);
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

pub fn touch_last_active(instance_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_or_create_instance_config(instance_dir)?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339)?;
    config.last_active = Some(now);
    write_instance_config(&instance_dir.join(INSTANCE_TOML_FILENAME), &config)?;
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
    loop {
        let config = load_or_create_instance_config(instance_dir)?;
        if config.vm_ipv4.is_some() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err("Timed out waiting for VM IPv4".into());
        }
        if start.elapsed().as_secs() % 5 == 0 {
            tracing::debug!("still waiting for vm ipv4");
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
            tracing::debug!(attempts, "ssh port not open yet");
            eprintln!(
                "[vibebox] waiting for ssh port on {ip} (attempt {attempts}/{SSH_CONNECT_RETRIES})"
            );
            if attempts >= SSH_CONNECT_RETRIES {
                return Err(
                    format!("ssh port not ready after {SSH_CONNECT_RETRIES} attempts").into(),
                );
            }
            thread::sleep(Duration::from_millis(SSH_CONNECT_DELAY_MS));
            continue;
        }

        eprintln!(
            "[vibebox] starting ssh to {ssh_user}@{ip} (attempt {attempts}/{SSH_CONNECT_RETRIES})"
        );
        tracing::info!(attempts, user = %ssh_user, ip = %ip, "starting ssh");
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
                eprintln!("[vibebox] ssh exited with status: {status}");
                break;
            }
            Ok(status) if status.code() == Some(255) => {
                eprintln!("[vibebox] ssh connection failed: {status}");
                if attempts >= SSH_CONNECT_RETRIES {
                    return Err(format!("ssh failed after {SSH_CONNECT_RETRIES} attempts").into());
                }
                thread::sleep(Duration::from_millis(500));
            }
            Ok(status) => {
                return Err(format!("ssh exited with status: {status}").into());
            }
            Err(err) => {
                return Err(format!("failed to start ssh: {err}").into());
            }
        }
    }

    Ok(())
}

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
    guest_dir: &str,
    key_name: &str,
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
        .replace("__KEY_PATH__", &key_path);
    let setup = vm::script_command_from_content("ssh_setup", &setup_script)
        .expect("ssh setup script contained invalid marker");

    vec![LoginAction::Send(setup)]
}

#[allow(dead_code)]
fn spawn_ssh_io(
    app: Arc<Mutex<AppState>>,
    config: Arc<Mutex<InstanceConfig>>,
    instance_dir: PathBuf,
    ssh_key: PathBuf,
    output_monitor: Arc<vm::OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
) -> vm::IoContext {
    let io_control = vm::IoControl::new();

    let log_path = instance_dir.join(SERIAL_LOG_NAME);
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok()
        .map(|file| Arc::new(Mutex::new(file)));

    let ssh_connected = Arc::new(AtomicBool::new(false));
    let ssh_started = Arc::new(AtomicBool::new(false));
    let ssh_ready = Arc::new(AtomicBool::new(false));
    let input_tx_holder: Arc<Mutex<Option<Sender<VmInput>>>> = Arc::new(Mutex::new(None));

    let instance_path = instance_dir.join(INSTANCE_TOML_FILENAME);
    let config_for_output = config.clone();
    let log_for_output = log_file.clone();
    let ssh_connected_for_output = ssh_connected.clone();
    let ssh_ready_for_output = ssh_ready.clone();

    let mut line_buf = String::new();

    let on_line = {
        let app = app.clone();
        move |line: &str| {
            if line == ":help" {
                if let Ok(mut locked) = app.lock() {
                    let _ = tui::render_commands_component(&mut locked);
                }
                return true;
            }
            false
        }
    };

    let on_output = move |bytes: &[u8]| {
        if ssh_connected_for_output.load(Ordering::SeqCst) {
            if let Some(log) = &log_for_output {
                if let Ok(mut file) = log.lock() {
                    let _ = file.write_all(bytes);
                }
            }
        }

        let text = String::from_utf8_lossy(bytes);
        line_buf.push_str(&text);

        while let Some(pos) = line_buf.find('\n') {
            let mut line = line_buf[..pos].to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            line_buf.drain(..=pos);

            let cleaned = line.trim_start_matches(|c: char| c == '\r' || c == ' ');

            if let Some(pos) = cleaned.find("VIBEBOX_IPV4=") {
                let ip_raw = &cleaned[(pos + "VIBEBOX_IPV4=".len())..];
                let ip = extract_ipv4(ip_raw).unwrap_or_default();
                if !ip.is_empty() {
                    if let Ok(mut cfg) = config_for_output.lock() {
                        if cfg.vm_ipv4.as_deref() != Some(ip.as_str()) {
                            cfg.vm_ipv4 = Some(ip.clone());
                            let _ = write_instance_config(&instance_path, &cfg);
                            eprintln!("[vibebox] detected vm IPv4: {ip}");
                        }
                    }
                }
            }

            if cleaned.contains("VIBEBOX_SSH_READY") {
                ssh_ready_for_output.store(true, Ordering::SeqCst);
                eprintln!("[vibebox] sshd ready");
            }
        }
    };

    let io_ctx = vm::spawn_vm_io_with_hooks(
        output_monitor,
        vm_output_fd,
        vm_input_fd,
        io_control.clone(),
        on_line,
        on_output,
    );

    *input_tx_holder.lock().unwrap() = Some(io_ctx.input_tx.clone());

    let ssh_ready_for_thread = ssh_ready.clone();
    let ssh_started_for_thread = ssh_started.clone();
    let ssh_connected_for_thread = ssh_connected.clone();
    let io_control_for_thread = io_control.clone();
    let ssh_key_for_thread = ssh_key.clone();
    let config_for_thread = config.clone();
    let input_tx_holder_for_thread = input_tx_holder.clone();

    thread::spawn(move || {
        loop {
            if ssh_started_for_thread.load(Ordering::SeqCst) {
                break;
            }
            if !ssh_ready_for_thread.load(Ordering::SeqCst) {
                thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }

            let ip = config_for_thread
                .lock()
                .ok()
                .and_then(|cfg| cfg.vm_ipv4.clone());
            let ssh_user = config_for_thread
                .lock()
                .map(|cfg| cfg.ssh_user.clone())
                .unwrap_or_else(|_| DEFAULT_SSH_USER.to_string());

            if let Some(ip) = ip {
                if ssh_started_for_thread
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    break;
                }

                eprintln!("[vibebox] starting ssh to {ssh_user}@{ip}");
                io_control_for_thread.request_terminal_restore();
                io_control_for_thread.set_forward_output(false);
                io_control_for_thread.set_forward_input(false);
                ssh_connected_for_thread.store(true, Ordering::SeqCst);

                let mut attempts = 0usize;
                loop {
                    attempts += 1;
                    if !ssh_port_open(&ip) {
                        eprintln!(
                            "[vibebox] waiting for ssh port on {ip} (attempt {attempts}/{SSH_CONNECT_RETRIES})"
                        );
                        if attempts >= SSH_CONNECT_RETRIES {
                            ssh_connected_for_thread.store(false, Ordering::SeqCst);
                            eprintln!(
                                "[vibebox] ssh port not ready after {SSH_CONNECT_RETRIES} attempts"
                            );
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(SSH_CONNECT_DELAY_MS));
                        continue;
                    }

                    eprintln!(
                        "[vibebox] starting ssh to {ssh_user}@{ip} (attempt {attempts}/{SSH_CONNECT_RETRIES})"
                    );
                    let status = Command::new("ssh")
                        .args([
                            "-i",
                            ssh_key_for_thread.to_str().unwrap_or(".vibebox/ssh_key"),
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
                        //  .env_remove("LANG")
                        .arg(format!("{ssh_user}@{ip}"))
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status();

                    match status {
                        Ok(status) if status.success() => {
                            ssh_connected_for_thread.store(false, Ordering::SeqCst);
                            eprintln!("[vibebox] ssh exited with status: {status}");
                            if let Some(tx) = input_tx_holder_for_thread.lock().unwrap().clone() {
                                let _ = tx.send(VmInput::Bytes(b"systemctl poweroff\n".to_vec()));
                            }
                            break;
                        }
                        Ok(status) if status.code() == Some(255) => {
                            eprintln!("[vibebox] ssh connection failed: {status}");
                            if attempts >= SSH_CONNECT_RETRIES {
                                ssh_connected_for_thread.store(false, Ordering::SeqCst);
                                eprintln!(
                                    "[vibebox] ssh failed after {SSH_CONNECT_RETRIES} attempts"
                                );
                                break;
                            }
                            thread::sleep(std::time::Duration::from_millis(500));
                            continue;
                        }
                        Ok(status) => {
                            ssh_connected_for_thread.store(false, Ordering::SeqCst);
                            eprintln!("[vibebox] ssh exited with status: {status}");
                            if let Some(tx) = input_tx_holder_for_thread.lock().unwrap().clone() {
                                let _ = tx.send(VmInput::Bytes(b"systemctl poweroff\n".to_vec()));
                            }
                            break;
                        }
                        Err(err) => {
                            ssh_connected_for_thread.store(false, Ordering::SeqCst);
                            eprintln!("[vibebox] failed to start ssh: {err}");
                            break;
                        }
                    }
                }
                break;
            }

            thread::sleep(std::time::Duration::from_millis(200));
        }
    });

    io_ctx
}
