use crate::session_manager::{GLOBAL_CACHE_DIR_NAME, INSTANCE_DIR_NAME};
use anyhow::{Context, Error, Result, bail};
use std::{
    env, fs,
    io::{self, Write},
    os::{
        fd::RawFd,
        unix::{
            io::{AsRawFd, IntoRawFd, OwnedFd},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use block2::RcBlock;
use bytesize::ByteSize;
use dispatch2::DispatchQueue;
use objc2::{AnyThread, rc::Retained, runtime::ProtocolObject};
use objc2_foundation::*;
use objc2_virtualization::*;

const DEBIAN_COMPRESSED_DISK_URL: &str = "https://cloud.debian.org/images/cloud/trixie/20260112-2355/debian-13-nocloud-arm64-20260112-2355.tar.xz";
const DEBIAN_COMPRESSED_SHA: &str = "6ab9be9e6834adc975268367f2f0235251671184345c34ee13031749fdfbf66fe4c3aafd949a2d98550426090e9ac645e79009c51eb0eefc984c15786570bb38";
const DEBIAN_COMPRESSED_SIZE_BYTES: u64 = 280901576;
const SHARED_DIRECTORIES_TAG: &str = "shared";
pub const PROJECT_GUEST_BASE: &str = "/usr/local/vibebox-mounts";

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LOGIN_EXPECT_TIMEOUT: Duration = Duration::from_secs(120);
const PROVISION_EXPECT_TIMEOUT: Duration = Duration::from_secs(900);

const PROVISION_SCRIPT: &str = include_str!("provision.sh");
const PROVISION_SCRIPT_NAME: &str = "provision.sh";
const RESIZE_DISK_SCRIPT: &str = include_str!("resize_disk.sh");
const DEFAULT_RAW_NAME: &str = "default.raw";
const INSTANCE_RAW_NAME: &str = "instance.raw";
const BASE_DISK_RAW_NAME: &str = "disk.raw";

#[derive(Clone)]
pub enum LoginAction {
    Expect {
        text: String,
        timeout: Duration,
    },
    ExpectEither {
        success: String,
        failure: String,
        timeout: Duration,
    },
    Send(String),
}
use crate::config::BoxConfig;
use LoginAction::*;

#[derive(Clone)]
pub struct DirectoryShare {
    host: PathBuf,
    guest: PathBuf,
    read_only: bool,
}

impl DirectoryShare {
    pub fn new(host: PathBuf, mut guest: PathBuf, read_only: bool) -> Result<Self> {
        if !host.exists() {
            bail!(format!("host path does not exist: {}", host.display()));
        }
        if !guest.is_absolute() {
            guest = PathBuf::from("/root").join(guest);
        }
        Ok(Self {
            host,
            guest,
            read_only,
        })
    }

    pub fn from_mount_spec(spec: &str) -> Result<Self> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            bail!(format!("invalid mount spec: {spec}"));
        }
        let host = expand_tilde_path(parts[0]);
        let guest = PathBuf::from(parts[1]);
        let read_only = if parts.len() == 3 {
            match parts[2] {
                "read-only" => true,
                "read-write" => false,
                _ => {
                    bail!(format!(
                        "Invalid mount mode '{}'; expected read-only or read-write",
                        parts[2]
                    ));
                }
            }
        } else {
            false
        };
        DirectoryShare::new(host, guest, read_only)
    }

    fn tag(&self) -> String {
        let path_str = self.host.to_string_lossy();
        let hash = path_str
            .bytes()
            .fold(5381u64, |h, b| h.wrapping_mul(33).wrapping_add(b as u64));
        let base_name = self
            .host
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or("share".into());
        format!("{}_{:016x}", base_name, hash)
    }
}

fn expand_tilde_path(value: &str) -> PathBuf {
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    } else if value == "~"
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(value)
}

pub struct VmArg {
    pub cpu_count: usize,
    pub ram_bytes: u64,
    pub disk_bytes: u64,
    pub no_default_mounts: bool,
    pub mounts: Vec<String>,
}

type StatusEmitter<'a> = dyn Fn(&str) + std::marker::Send + Sync + 'a;

fn emit_status(status: Option<&StatusEmitter<'_>>, message: &str) {
    if let Some(status) = status {
        status(message);
    }
}

pub fn run_with_args<F>(args: VmArg, io_handler: F) -> Result<()>
where
    F: FnOnce(Arc<OutputMonitor>, OwnedFd, OwnedFd) -> IoContext,
{
    run_with_args_and_extras(args, io_handler, Vec::new(), Vec::new(), None)
}

pub fn run_with_args_and_extras<F>(
    args: VmArg,
    io_handler: F,
    extra_login_actions: Vec<LoginAction>,
    extra_directory_shares: Vec<DirectoryShare>,
    status: Option<&StatusEmitter<'_>>,
) -> Result<()>
where
    F: FnOnce(Arc<OutputMonitor>, OwnedFd, OwnedFd) -> IoContext,
{
    ensure_signed();

    let project_root = env::current_dir()?;
    let project_name = project_root
        .file_name()
        .with_context(|| "Project directory has no name")?
        .to_string_lossy()
        .into_owned();

    let home = env::var("HOME").map(PathBuf::from)?;
    let cache_home = env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".cache"));
    let cache_dir = cache_home.join(GLOBAL_CACHE_DIR_NAME);
    let guest_mise_cache = cache_dir.join(".guest-mise-cache");

    let instance_dir = project_root.join(INSTANCE_DIR_NAME);
    fs::create_dir_all(&instance_dir)?;
    emit_status(status, "preparing VM image...");
    tracing::info!("preparing VM image...");
    let provision_log = instance_dir.join("provision.log");

    let basename_compressed = DEBIAN_COMPRESSED_DISK_URL.rsplit('/').next().unwrap();
    let base_compressed = cache_dir.join(basename_compressed);
    let base_raw = cache_dir.join(format!(
        "{}.raw",
        basename_compressed.trim_end_matches(".tar.xz")
    ));

    let default_raw = cache_dir.join(DEFAULT_RAW_NAME);
    let instance_raw = instance_dir.join(INSTANCE_RAW_NAME);

    // Prepare system-wide directories
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&guest_mise_cache)?;

    let mise_directory_share =
        DirectoryShare::new(guest_mise_cache, "/root/.local/share/mise".into(), false)?;

    ensure_default_image(
        &base_raw,
        &base_compressed,
        &default_raw,
        std::slice::from_ref(&mise_directory_share),
        Some(&provision_log),
        status,
    )?;
    let _ = ensure_instance_disk(
        &instance_raw,
        &default_raw,
        ByteSize(args.disk_bytes),
        status,
    )?;
    let base_size = fs::metadata(&default_raw)?.len();
    let instance_size = fs::metadata(&instance_raw)?.len();
    let needs_resize = instance_size > base_size;
    let disk_path = instance_raw;

    let mut login_actions = Vec::new();
    let mut directory_shares = Vec::new();

    if !args.no_default_mounts {
        let project_guest_dir = PathBuf::from(PROJECT_GUEST_BASE).join(project_name);
        login_actions.push(Send(format!("cd {}", project_guest_dir.display())));

        // discourage read/write of .git folder from within the VM. note that this isn't secure, since the VM runs as root and could unmount this.
        // I couldn't find an alternative way to do this --- the MacOS sandbox doesn't apply to the Apple Virtualization system
        if project_root.join(".git").exists() {
            login_actions.push(Send(r"mount -t tmpfs tmpfs .git/".into()));
        }

        directory_shares.push(mise_directory_share);
    }

    directory_shares.extend(extra_directory_shares);

    for spec in &args.mounts {
        directory_shares.push(DirectoryShare::from_mount_spec(spec)?);
    }

    if needs_resize {
        let resize_cmd = script_command_from_content("resize_disk", RESIZE_DISK_SCRIPT)?;
        login_actions.push(Send(resize_cmd));
    }

    if let Some(motd_action) = motd_login_action(&directory_shares) {
        login_actions.push(motd_action);
    }

    login_actions.extend(extra_login_actions);

    run_vm_with_io(
        &disk_path,
        &login_actions,
        &directory_shares[..],
        args.cpu_count,
        args.ram_bytes,
        status,
        io_handler,
    )
}

pub fn script_command_from_content(label: &str, script: &str) -> Result<String> {
    let marker = "VIBE_SCRIPT_EOF";
    let guest_dir = "/tmp/vibe-scripts";
    let guest_path = format!("{guest_dir}/{label}.sh");
    let script_body = match script.split_once('\n') {
        Some((first, rest)) if first.starts_with("#!") => rest,
        _ => script,
    };
    let wrapped_script = format!(
        r#"#!/usr/bin/env bash
set -Eeuo pipefail
__vibebox_err_reported=0
__vibebox_report_error() {{
  local rc="$1"
  local line="$2"
  if [ "$__vibebox_err_reported" -eq 0 ]; then
    echo "VIBEBOX_SCRIPT_ERROR:{label}:${{line}}:${{rc}}"
    __vibebox_err_reported=1
  fi
}}
trap '__vibebox_report_error "$?" "${{LINENO}}"' ERR
trap 'rc="$?"; if [ "$rc" -ne 0 ]; then __vibebox_report_error "$rc" "${{LINENO}}"; fi' EXIT
{script_body}
"#
    );
    let command = format!(
        "mkdir -p {guest_dir}\ncat >{guest_path} <<'{marker}'\n{wrapped_script}\n{marker}\nchmod +x {guest_path}\n{guest_path}"
    );
    if script.contains(marker) {
        bail!(format!(
            "Script '{label}' contains marker '{marker}', cannot safely upload"
        ));
    }
    Ok(command)
}

fn motd_login_action(directory_shares: &[DirectoryShare]) -> Option<LoginAction> {
    if directory_shares.is_empty() {
        return Some(Send("clear".into()));
    }

    let host_header = "Host";
    let guest_header = "Guest";
    let mode_header = "Mode";
    let mut host_width = host_header.len();
    let mut guest_width = guest_header.len();
    let mut mode_width = mode_header.len();
    let mut rows = Vec::with_capacity(directory_shares.len());

    for share in directory_shares {
        let host = share.host.to_string_lossy().into_owned();
        let guest = share.guest.to_string_lossy().into_owned();
        let mode = if share.read_only {
            "read-only"
        } else {
            "read-write"
        }
        .to_string();
        host_width = host_width.max(host.len());
        guest_width = guest_width.max(guest.len());
        mode_width = mode_width.max(mode.len());
        rows.push((host, guest, mode));
    }

    let mut output = String::new();
    output.push_str(&format!(
        "{host_header:<host_width$}  {guest_header:<guest_width$}  {mode_header}\n",
        host_width = host_width
    ));
    output.push_str(&format!(
        "{:-<host_width$}  {:-<guest_width$}  {:-<mode_width$}\n",
        "",
        "",
        "",
        host_width = host_width,
        guest_width = guest_width,
        mode_width = mode_width
    ));

    for (host, guest, mode) in rows {
        output.push_str(&format!(
            "{host:<host_width$}  {guest:<guest_width$}  {mode}\n"
        ));
    }

    let command = format!("clear && cat <<'VIBE_MOTD'\n{output}\nVIBE_MOTD");
    Some(Send(command))
}

#[derive(PartialEq, Eq)]
enum WaitResult {
    Timeout,
    Found,
}

#[derive(PartialEq, Eq)]
enum WaitAnyResult {
    Timeout,
    Found(usize),
}

pub enum VmInput {
    Bytes(Vec<u8>),
    Shutdown,
}

enum VmOutput {
    LoginActionTimeout { action: String, timeout: Duration },
    LoginActionFailed { action: String, reason: String },
}

#[derive(Default)]
pub struct OutputMonitor {
    buffer: Mutex<String>,
    condvar: Condvar,
}

impl OutputMonitor {
    fn push(&self, bytes: &[u8]) {
        self.buffer
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(bytes));
        self.condvar.notify_all();
    }

    fn wait_for(&self, needle: &str, timeout: Duration) -> WaitResult {
        let (_unused, timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                if let Some((_, remaining)) = buf.split_once(needle) {
                    *buf = remaining.to_string();
                    false
                } else {
                    true
                }
            })
            .unwrap();

        if timeout_result.timed_out() {
            WaitResult::Timeout
        } else {
            WaitResult::Found
        }
    }

    fn wait_for_any(&self, needles: &[&str], timeout: Duration) -> WaitAnyResult {
        let mut found: Option<usize> = None;
        let (_unused, timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                if let Some((pos, idx, len)) = find_any(buf, needles) {
                    *buf = buf[(pos + len)..].to_string();
                    found = Some(idx);
                    false
                } else {
                    true
                }
            })
            .unwrap();

        if timeout_result.timed_out() {
            WaitAnyResult::Timeout
        } else {
            WaitAnyResult::Found(found.unwrap_or(0))
        }
    }
}

fn find_any(buf: &str, needles: &[&str]) -> Option<(usize, usize, usize)> {
    let mut best: Option<(usize, usize, usize)> = None; // (pos, idx, len)
    for (idx, needle) in needles.iter().enumerate() {
        if let Some(pos) = buf.find(needle) {
            let candidate = (pos, idx, needle.len());
            if best.is_none_or(|b| candidate.0 < b.0) {
                best = Some(candidate);
            }
        }
    }
    best
}

#[derive(Debug)]
pub struct IoControl {
    forward_input: AtomicBool,
    forward_output: AtomicBool,
    restore_terminal: AtomicBool,
}

impl IoControl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            forward_input: AtomicBool::new(true),
            forward_output: AtomicBool::new(true),
            restore_terminal: AtomicBool::new(false),
        })
    }

    pub fn set_forward_input(&self, enabled: bool) {
        self.forward_input.store(enabled, Ordering::SeqCst);
    }

    pub fn set_forward_output(&self, enabled: bool) {
        self.forward_output.store(enabled, Ordering::SeqCst);
    }

    pub fn request_terminal_restore(&self) {
        self.restore_terminal.store(true, Ordering::SeqCst);
    }

    fn forward_input(&self) -> bool {
        self.forward_input.load(Ordering::SeqCst)
    }

    fn forward_output(&self) -> bool {
        self.forward_output.load(Ordering::SeqCst)
    }

    fn take_restore_terminal(&self) -> bool {
        self.restore_terminal
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

fn ensure_base_image(
    base_raw: &Path,
    base_compressed: &Path,
    status: Option<&StatusEmitter<'_>>,
) -> Result<()> {
    if base_raw.exists() {
        return Ok(());
    }

    if !base_compressed.exists()
        || fs::metadata(base_compressed).map(|m| m.len())? < DEBIAN_COMPRESSED_SIZE_BYTES
    {
        emit_status(status, "downloading base image...");
        tracing::info!("downloading base image");
        let status = Command::new("curl")
            .args([
                "--continue-at",
                "-",
                "--compressed",
                "--location",
                "--fail",
                "-o",
                &base_compressed.to_string_lossy(),
                DEBIAN_COMPRESSED_DISK_URL,
            ])
            .status()?;
        if !status.success() {
            bail!("failed to download base image");
        }
    }

    // Check SHA
    {
        emit_status(status, "verifying base image...");
        tracing::info!("verifying base image...");
        let input = format!("{}  {}\n", DEBIAN_COMPRESSED_SHA, base_compressed.display());

        let mut child = Command::new("/usr/bin/shasum")
            .args(["--algorithm", "512", "--check"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("failed to spawn shasum");

        child
            .stdin
            .take()
            .expect("failed to open stdin")
            .write_all(input.as_bytes())
            .expect("failed to write to stdin");

        let status = child.wait().expect("failed to wait on child");
        if !status.success() {
            bail!(format!(
                "SHA validation failed for {DEBIAN_COMPRESSED_DISK_URL}"
            ));
        }
    }

    emit_status(status, "decompressing base image...");
    tracing::info!("decompressing base image...");
    let status = Command::new("tar")
        .args([
            "-xOf",
            &base_compressed.to_string_lossy(),
            BASE_DISK_RAW_NAME,
        ])
        .stdout(fs::File::create(base_raw)?)
        .status()?;

    if !status.success() {
        bail!("Failed to decompress base image");
    }

    Ok(())
}

fn ensure_default_image(
    base_raw: &Path,
    base_compressed: &Path,
    default_raw: &Path,
    directory_shares: &[DirectoryShare],
    provision_log: Option<&Path>,
    status: Option<&StatusEmitter<'_>>,
) -> Result<()> {
    if default_raw.exists() {
        return Ok(());
    }

    ensure_base_image(base_raw, base_compressed, status)?;

    emit_status(status, "configuring base image...");
    tracing::info!("configuring base image...");
    fs::copy(base_raw, default_raw)?;

    let provision_command = script_command_from_content(PROVISION_SCRIPT_NAME, PROVISION_SCRIPT)?;
    let provision_actions = [
        Send(provision_command),
        ExpectEither {
            success: "VIBEBOX_PROVISION_OK".to_string(),
            failure: "VIBEBOX_PROVISION_FAILED".to_string(),
            timeout: PROVISION_EXPECT_TIMEOUT,
        },
    ];
    let provision_result = if let Some(log_path) = provision_log {
        let log_path = log_path.to_path_buf();
        run_vm_with_io(
            default_raw,
            &provision_actions,
            directory_shares,
            BoxConfig::default().cpu_count,
            BoxConfig::default().ram_size.as_u64(),
            status,
            move |output_monitor, vm_output_fd, vm_input_fd| {
                spawn_vm_io_with_log(output_monitor, vm_output_fd, vm_input_fd, log_path)
            },
        )
    } else {
        run_vm(
            default_raw,
            &provision_actions,
            directory_shares,
            BoxConfig::default().cpu_count,
            BoxConfig::default().ram_size.as_u64(),
            status,
        )
    };

    if let Err(err) = provision_result {
        let _ = fs::remove_file(default_raw);
        return Err(err);
    }

    Ok(())
}

fn ensure_instance_disk(
    instance_raw: &Path,
    template_raw: &Path,
    target_bytes: ByteSize,
    status: Option<&StatusEmitter<'_>>,
) -> Result<bool> {
    if instance_raw.exists() {
        let current_size = ByteSize(fs::metadata(instance_raw)?.len());
        if current_size != target_bytes {
            let current_gb = current_size;
            let target_gb = target_bytes;
            tracing::warn!(
                "instance disk size does not match config (current {}, config {}); disk_gb applies only on init. Run `vibebox reset` to recreate or set disk_gb to match; using existing disk.",
                current_gb,
                target_gb
            );
        }
        return Ok(false);
    }

    let template_size = ByteSize(fs::metadata(template_raw)?.len());
    if target_bytes < template_size {
        bail!(format!(
            "Requested disk size {} is smaller than base image size {}",
            target_bytes, template_size
        ));
    }
    let target_size = target_bytes;
    let needs_resize = target_size > template_size;

    emit_status(status, "creating instance disk...");
    tracing::info!("creating instance disk...");
    tracing::info!(path = %template_raw.display(), "creating instance disk");
    fs::create_dir_all(instance_raw.parent().unwrap())?;
    if target_size == template_size {
        fs::copy(template_raw, instance_raw)?;
        return Ok(needs_resize);
    }

    let mut dst = fs::File::create(instance_raw)?;
    dst.set_len(target_size.as_u64())?;
    let mut src = fs::File::open(template_raw)?;
    std::io::copy(&mut src, &mut dst)?;
    Ok(needs_resize)
}

pub struct IoContext {
    pub input_tx: Sender<VmInput>,
    wakeup_write: OwnedFd,
    stdin_thread: thread::JoinHandle<()>,
    mux_thread: thread::JoinHandle<()>,
    stdout_thread: thread::JoinHandle<()>,
}

pub fn create_pipe() -> (OwnedFd, OwnedFd) {
    let (read_stream, write_stream) = UnixStream::pair().expect("Failed to create socket pair");
    (read_stream.into(), write_stream.into())
}

pub fn spawn_vm_io_with_hooks<
    F: FnMut(&str) -> bool + std::marker::Send + 'static,
    G: FnMut(&[u8]) + std::marker::Send + 'static,
>(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
    io_control: Arc<IoControl>,
    mut on_line: F,
    mut on_output: G,
) -> IoContext {
    let (input_tx, input_rx): (Sender<VmInput>, Receiver<VmInput>) = mpsc::channel();

    // raw_guard is set when we've put the user's terminal into raw mode because we've attached stdin/stdout to the VM.
    let raw_guard = Arc::new(Mutex::new(None));

    let (wakeup_read, wakeup_write) = create_pipe();

    enum PollResult<'a> {
        Ready(&'a [u8]),
        Spurious,
        Shutdown,
        Error,
    }

    fn poll_with_wakeup<'a>(
        main_fd: RawFd,
        wakeup_fd: RawFd,
        buf: &'a mut [u8],
        timeout_ms: i32,
    ) -> PollResult<'a> {
        let mut fds = [
            libc::pollfd {
                fd: main_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if ret == 0 {
            PollResult::Spurious
        } else if ret < 0 {
            PollResult::Error
        } else if fds[1].revents & libc::POLLIN != 0 {
            PollResult::Shutdown
        } else if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(main_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 {
                PollResult::Error
            } else if n == 0 {
                PollResult::Shutdown
            } else {
                PollResult::Ready(&buf[..(n as usize)])
            }
        } else {
            PollResult::Spurious
        }
    }

    fn poll_wakeup_only(wakeup_fd: RawFd, timeout_ms: i32) -> bool {
        let mut fds = [libc::pollfd {
            fd: wakeup_fd,
            events: libc::POLLIN,
            revents: 0,
        }];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        ret > 0 && fds[0].revents & libc::POLLIN != 0
    }

    // Copies from stdin to the VM; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdin_thread = thread::spawn({
        let input_tx = input_tx.clone();
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();
        let io_control = io_control.clone();

        move || {
            let mut buf = [0u8; 1024];
            let mut pending_command: Vec<u8> = Vec::new();
            let mut command_mode = false;
            loop {
                if !io_control.forward_input() {
                    if poll_wakeup_only(wakeup_read.as_raw_fd(), 100) {
                        break;
                    }
                    continue;
                }

                match poll_with_wakeup(libc::STDIN_FILENO, wakeup_read.as_raw_fd(), &mut buf, -1) {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        let mut send_buf: Vec<u8> = Vec::new();
                        for &b in bytes {
                            if pending_command.is_empty() && !command_mode && b == b':' {
                                command_mode = true;
                            }

                            if command_mode {
                                pending_command.push(b);
                            } else {
                                send_buf.push(b);
                            }

                            if b == b'\n' && command_mode {
                                let line = String::from_utf8_lossy(&pending_command);
                                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                                let consumed = on_line(trimmed);
                                if !consumed {
                                    send_buf.extend_from_slice(&pending_command);
                                }
                                pending_command.clear();
                                command_mode = false;
                            }
                        }
                        if raw_guard.lock().unwrap().is_none() {
                            continue;
                        }
                        if !send_buf.is_empty() && input_tx.send(VmInput::Bytes(send_buf)).is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Copies VM output to stdout; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdout_thread = thread::spawn({
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();
        let io_control = io_control.clone();

        move || {
            let mut buf = [0u8; 1024];
            loop {
                if io_control.take_restore_terminal() {
                    let mut guard = raw_guard.lock().unwrap();
                    *guard = None;
                }
                match poll_with_wakeup(
                    vm_output_fd.as_raw_fd(),
                    wakeup_read.as_raw_fd(),
                    &mut buf,
                    100,
                ) {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        if io_control.forward_output() {
                            // enable raw mode if we haven't already
                            if raw_guard.lock().unwrap().is_none()
                                && let Ok(guard) = enable_raw_mode(libc::STDIN_FILENO)
                            {
                                *raw_guard.lock().unwrap() = Some(guard);
                            }

                            let mut stdout = io::stdout().lock();
                            if stdout.write_all(bytes).is_err() {
                                break;
                            }
                            let _ = stdout.flush();
                        }
                        output_monitor.push(bytes);
                        on_output(bytes);
                    }
                }
            }
        }
    });

    // Copies data from mpsc channel into VM, so vibe can "type" stuff and run scripts.
    let mux_thread = thread::spawn(move || {
        let mut vm_writer = fs::File::from(vm_input_fd);
        loop {
            match input_rx.recv() {
                Ok(VmInput::Bytes(data)) => {
                    if vm_writer.write_all(&data).is_err() {
                        break;
                    }
                }
                Ok(VmInput::Shutdown) => break,
                Err(_) => break,
            }
        }
    });

    IoContext {
        input_tx,
        wakeup_write,
        stdin_thread,
        mux_thread,
        stdout_thread,
    }
}

pub fn spawn_vm_io_with_line_handler<F>(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
    on_line: F,
) -> IoContext
where
    F: FnMut(&str) -> bool + std::marker::Send + 'static,
{
    spawn_vm_io_with_hooks(
        output_monitor,
        vm_output_fd,
        vm_input_fd,
        IoControl::new(),
        on_line,
        |_| {},
    )
}

pub fn spawn_vm_io(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
) -> IoContext {
    spawn_vm_io_with_line_handler(output_monitor, vm_output_fd, vm_input_fd, |_| false)
}

fn spawn_vm_io_with_log(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
    log_path: PathBuf,
) -> IoContext {
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .ok()
        .map(|file| Arc::new(Mutex::new(file)));

    spawn_vm_io_with_hooks(
        output_monitor,
        vm_output_fd,
        vm_input_fd,
        IoControl::new(),
        |_| false,
        move |bytes| {
            if let Some(log) = &log_file
                && let Ok(mut file) = log.lock()
            {
                let _ = file.write_all(bytes);
            }
        },
    )
}

impl IoContext {
    pub fn shutdown(self) {
        let _ = self.input_tx.send(VmInput::Shutdown);
        unsafe { libc::write(self.wakeup_write.as_raw_fd(), b"x".as_ptr() as *const _, 1) };
        let _ = self.stdin_thread.join();
        let _ = self.stdout_thread.join();
        let _ = self.mux_thread.join();
    }
}

fn create_vm_configuration(
    disk_path: &Path,
    directory_shares: &[DirectoryShare],
    vm_reads_from_fd: OwnedFd,
    vm_writes_to_fd: OwnedFd,
    cpu_count: usize,
    ram_bytes: u64,
) -> Result<Retained<VZVirtualMachineConfiguration>> {
    unsafe {
        let platform =
            VZGenericPlatformConfiguration::init(VZGenericPlatformConfiguration::alloc());

        let boot_loader = VZEFIBootLoader::init(VZEFIBootLoader::alloc());
        let variable_store = load_efi_variable_store()?;
        boot_loader.setVariableStore(Some(&variable_store));

        let config = VZVirtualMachineConfiguration::new();
        config.setPlatform(&platform);
        config.setBootLoader(Some(&boot_loader));
        config.setCPUCount(cpu_count as NSUInteger);
        config.setMemorySize(ram_bytes);

        config.setNetworkDevices(&NSArray::from_retained_slice(&[{
            let network_device = VZVirtioNetworkDeviceConfiguration::new();
            network_device.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
            Retained::into_super(network_device)
        }]));

        config.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )]));

        ////////////////////////////
        // Disks
        {
            let disk_attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &nsurl_from_path(disk_path).unwrap(),
            false,
            VZDiskImageCachingMode::Automatic,
            VZDiskImageSynchronizationMode::Full,
        )?;

            let disk_device = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                &disk_attachment,
            );

            let storage_devices: Retained<NSArray<_>> =
                NSArray::from_retained_slice(&[Retained::into_super(disk_device)]);

            config.setStorageDevices(&storage_devices);
        };

        ////////////////////////////
        // Directory shares

        if !directory_shares.is_empty() {
            let directories: Retained<NSMutableDictionary<NSString, VZSharedDirectory>> =
                NSMutableDictionary::new();

            for share in directory_shares.iter() {
                assert!(
                    share.host.is_dir(),
                    "path does not exist or is not a directory: {:?}",
                    share.host
                );

                let url = nsurl_from_path(&share.host)?;
                let shared_directory = VZSharedDirectory::initWithURL_readOnly(
                    VZSharedDirectory::alloc(),
                    &url,
                    share.read_only,
                );

                let key = NSString::from_str(&share.tag());
                directories.setObject_forKey(&*shared_directory, ProtocolObject::from_ref(&*key));
            }

            let multi_share = VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &directories,
            );

            let device = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(SHARED_DIRECTORIES_TAG),
            );
            device.setShare(Some(&multi_share));

            let share_devices = NSArray::from_retained_slice(&[device.into_super()]);
            config.setDirectorySharingDevices(&share_devices);
        }

        ////////////////////////////
        // Serial port
        {
            let ns_read_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_reads_from_fd.into_raw_fd(),
                true,
            );

            let ns_write_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_writes_to_fd.into_raw_fd(),
                true,
            );

            let serial_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    Some(&ns_read_handle),
                    Some(&ns_write_handle),
                );
            let serial_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_port.setAttachment(Some(&serial_attach));

            let serial_ports: Retained<NSArray<_>> =
                NSArray::from_retained_slice(&[Retained::into_super(serial_port)]);

            config.setSerialPorts(&serial_ports);
        }

        ////////////////////////////
        // Validate
        config.validateWithError().map_err(|e| {
            io::Error::other(format!(
                "invalid VM configuration: {:?}",
                e.localizedDescription()
            ))
        })?;

        Ok(config)
    }
}

fn load_efi_variable_store() -> Result<Retained<VZEFIVariableStore>> {
    unsafe {
        let temp_dir = env::temp_dir();
        let temp_path = temp_dir.join(format!("efi_variable_store_{}.efivars", std::process::id()));
        let url = nsurl_from_path(&temp_path)?;
        let options = VZEFIVariableStoreInitializationOptions::AllowOverwrite;
        let store = VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
            VZEFIVariableStore::alloc(),
            &url,
            options,
        )?;
        Ok(store)
    }
}

fn spawn_login_actions_thread(
    login_actions: Vec<LoginAction>,
    output_monitor: Arc<OutputMonitor>,
    input_tx: Sender<VmInput>,
    vm_output_tx: Sender<VmOutput>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for a in login_actions {
            match a {
                Expect { text, timeout } => {
                    if WaitResult::Timeout == output_monitor.wait_for(&text, timeout) {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                            action: format!("expect '{}'", text),
                            timeout,
                        });
                        return;
                    }
                }
                ExpectEither {
                    success,
                    failure,
                    timeout,
                } => match output_monitor.wait_for_any(&[&success, &failure], timeout) {
                    WaitAnyResult::Found(0) => {}
                    WaitAnyResult::Found(_) => {
                        let _ = vm_output_tx.send(VmOutput::LoginActionFailed {
                            action: format!("expect '{}'", success),
                            reason: format!("saw failure marker '{}'", failure),
                        });
                        return;
                    }
                    WaitAnyResult::Timeout => {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                            action: format!("expect '{}'", success),
                            timeout,
                        });
                        return;
                    }
                },
                Send(mut text) => {
                    text.push('\n'); // Type the newline so the command is actually submitted.
                    input_tx.send(VmInput::Bytes(text.into_bytes())).unwrap();
                }
            }
        }
    })
}

fn run_vm_with_io<F>(
    disk_path: &Path,
    login_actions: &[LoginAction],
    directory_shares: &[DirectoryShare],
    cpu_count: usize,
    ram_bytes: u64,
    status: Option<&StatusEmitter<'_>>,
    io_handler: F,
) -> Result<()>
where
    F: FnOnce(Arc<OutputMonitor>, OwnedFd, OwnedFd) -> IoContext,
{
    let (vm_reads_from, we_write_to) = create_pipe();
    let (we_read_from, vm_writes_to) = create_pipe();

    let config = create_vm_configuration(
        disk_path,
        directory_shares,
        vm_reads_from,
        vm_writes_to,
        cpu_count,
        ram_bytes,
    )?;

    let queue = DispatchQueue::main();

    let vm = unsafe {
        VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), &config, queue)
    };

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let completion_handler = RcBlock::new(move |error: *mut NSError| {
        if error.is_null() {
            let _ = tx.send(Ok(()));
        } else {
            let err = unsafe { &*error };
            let _ = tx.send(Err(format!("{:?}", err.localizedDescription())));
        }
    });

    unsafe {
        vm.startWithCompletionHandler(&completion_handler);
    }

    let start_deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < start_deadline {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            )
        };

        match rx.try_recv() {
            Ok(result) => {
                result.map_err(|e| Error::msg(format!("Failed to start VM: {}", e)))?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                bail!("VM start channel disconnected");
            }
        }
    }

    if Instant::now() >= start_deadline {
        bail!("Timed out waiting for VM to start");
    }

    emit_status(status, "vm booting... go vibecoder!");
    tracing::info!("vm booting... go vibecoder!");
    tracing::info!("vm booting");

    let output_monitor = Arc::new(OutputMonitor::default());
    let io_ctx = io_handler(output_monitor.clone(), we_read_from, we_write_to);

    let mut all_login_actions = vec![
        Expect {
            text: "login: ".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
        Send("root".to_string()),
        Expect {
            text: "~#".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
    ];

    if !directory_shares.is_empty() {
        all_login_actions.push(Send("mkdir -p /mnt/shared".into()));
        all_login_actions.push(Send(format!(
            "mount -t virtiofs {} /mnt/shared",
            SHARED_DIRECTORIES_TAG
        )));

        for share in directory_shares {
            let staging = format!("/mnt/shared/{}", share.tag());
            let guest = share.guest.to_string_lossy();
            all_login_actions.push(Send(format!("mkdir -p {}", guest)));
            all_login_actions.push(Send(format!("mount --bind {} {}", staging, guest)));
        }
    }

    for a in login_actions {
        all_login_actions.push(a.clone())
    }

    let (vm_output_tx, vm_output_rx) = mpsc::channel::<VmOutput>();
    let login_actions_thread = spawn_login_actions_thread(
        all_login_actions,
        output_monitor.clone(),
        io_ctx.input_tx.clone(),
        vm_output_tx,
    );

    let mut last_state = None;
    let mut exit_result: Result<(), String> = Ok(());
    loop {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.2),
            )
        };

        let state = unsafe { vm.state() };
        if last_state != Some(state) {
            //eprintln!("[state] {:?}", state);
            last_state = Some(state);
        }
        match vm_output_rx.try_recv() {
            Ok(VmOutput::LoginActionTimeout { action, timeout }) => {
                exit_result = Err(format!(
                    "Login action ({}) timed out after {:?}; shutting down.",
                    action, timeout
                ));
                unsafe {
                    if vm.canRequestStop() {
                        if let Err(err) = vm.requestStopWithError() {
                            tracing::error!(error = ?err, "failed to request VM stop");
                        }
                    } else if vm.canStop() {
                        let handler = RcBlock::new(|_error: *mut NSError| {});
                        vm.stopWithCompletionHandler(&handler);
                    }
                }
                break;
            }
            Ok(VmOutput::LoginActionFailed { action, reason }) => {
                exit_result = Err(format!(
                    "Login action ({}) failed: {}; shutting down.",
                    action, reason
                ));
                unsafe {
                    if vm.canRequestStop() {
                        if let Err(err) = vm.requestStopWithError() {
                            tracing::error!(error = ?err, "failed to request VM stop");
                        }
                    } else if vm.canStop() {
                        let handler = RcBlock::new(|_error: *mut NSError| {});
                        vm.stopWithCompletionHandler(&handler);
                    }
                }
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        if state != VZVirtualMachineState::Running {
            //eprintln!("VM stopped with state: {:?}", state);
            break;
        }
    }

    let _ = login_actions_thread.join();

    io_ctx.shutdown();

    exit_result.map_err(Error::msg)
}

fn run_vm(
    disk_path: &Path,
    login_actions: &[LoginAction],
    directory_shares: &[DirectoryShare],
    cpu_count: usize,
    ram_bytes: u64,
    status: Option<&StatusEmitter<'_>>,
) -> Result<()> {
    run_vm_with_io(
        disk_path,
        login_actions,
        directory_shares,
        cpu_count,
        ram_bytes,
        status,
        spawn_vm_io,
    )
}

fn nsurl_from_path(path: &Path) -> Result<Retained<NSURL>> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let ns_path = NSString::from_str(
        abs_path
            .to_str()
            .with_context(|| "non-UTF8 path encountered while building NSURL")?,
    );
    Ok(NSURL::fileURLWithPath(&ns_path))
}

fn enable_raw_mode(fd: i32) -> io::Result<RawModeGuard> {
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };

    if unsafe { libc::tcgetattr(fd, &mut attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let original = attributes;

    // Disable translation of carriage return to newline on input
    attributes.c_iflag &= !libc::ICRNL;
    // Disable canonical mode (line buffering), echo, and signal generation
    attributes.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
    attributes.c_cc[libc::VMIN] = 0;
    attributes.c_cc[libc::VTIME] = 1;

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(RawModeGuard { fd, original })
}

struct RawModeGuard {
    fd: i32,
    original: libc::termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

// Ensure the running binary has com.apple.security.virtualization entitlements by checking and, if not, signing and relaunching.
pub fn ensure_signed() {
    if env::var("VIBEBOX_SKIP_CODESIGN").as_deref() == Ok("1") {
        return;
    }
    let exe = env::current_exe().expect("failed to get current exe path");
    let exe_str = exe.to_str().expect("exe path not valid utf-8");

    let has_required_entitlements = {
        let output = Command::new("codesign")
            .args(["-d", "--entitlements", "-", "--xml", exe.to_str().unwrap()])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("com.apple.security.virtualization")
            }
            _ => false,
        }
    };

    if has_required_entitlements {
        return;
    }

    const ENTITLEMENTS: &str = include_str!("entitlements.plist");
    let entitlements_path = env::temp_dir().join("entitlements.plist");
    fs::write(&entitlements_path, ENTITLEMENTS).expect("failed to write entitlements");

    let output = Command::new("codesign")
        .args([
            "--sign",
            "-",
            "--force",
            "--entitlements",
            entitlements_path.to_str().unwrap(),
            exe_str,
        ])
        .output();

    let _ = fs::remove_file(&entitlements_path);

    match output {
        Ok(o) if o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.trim().is_empty() {
                tracing::debug!(codesign_stderr = %stderr.trim(), "codesign output");
            }
            let err = Command::new(&exe).args(env::args_os().skip(1)).exec();
            tracing::error!(error = %err, "failed to re-exec after signing");
            std::process::exit(1);
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            tracing::error!(status = %o.status, codesign_stderr = %stderr.trim(), "codesign failed");
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to run codesign");
            std::process::exit(1);
        }
    }
}
