use std::{
    env, fs,
    io::{Read, Write},
    os::unix::{
        fs::FileTypeExt,
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::CONFIG_PATH_ENV,
    instance::VM_ROOT_LOG_NAME,
    instance::{
        DEFAULT_SSH_USER, InstanceConfig, build_ssh_login_actions, ensure_instance_dir,
        ensure_ssh_keypair, extract_ipv4, load_or_create_instance_config, write_instance_config,
    },
    session_manager::{
        GLOBAL_DIR_NAME, INSTANCE_FILENAME, VM_MANAGER_PID_NAME, VM_MANAGER_SOCKET_NAME,
    },
    vm::{self, DirectoryShare, LoginAction, PROJECT_GUEST_BASE, VmInput},
};

const VM_MANAGER_LOCK_NAME: &str = "vm.lock";
const VM_MANAGER_LOG_NAME: &str = "vm_manager.log";
const SHUTDOWN_RETRY_MS: u64 = 500;
#[cfg(test)]
const HARD_SHUTDOWN_TIMEOUT_MS: u64 = 1_000;
#[cfg(not(test))]
const HARD_SHUTDOWN_TIMEOUT_MS: u64 = 12_000;

#[cfg(not(test))]
fn force_exit(_reason: &str) -> ! {
    std::process::exit(1);
}

#[cfg(test)]
fn force_exit(reason: &str) -> ! {
    panic!("{reason}");
}

pub fn ensure_manager(
    raw_args: &[std::ffi::OsString],
    auto_shutdown_ms: u64,
    config_path: Option<&Path>,
) -> Result<UnixStream, Box<dyn std::error::Error>> {
    let project_root = env::current_dir()?;
    tracing::debug!(root = %project_root.display(), "ensure vm manager");
    let instance_dir = ensure_instance_dir(&project_root)?;
    cleanup_stale_manager(&instance_dir);
    let socket_path = instance_dir.join(VM_MANAGER_SOCKET_NAME);

    if let Ok(stream) = UnixStream::connect(&socket_path) {
        send_client_pid(&stream);
        tracing::info!(path = %socket_path.display(), "connected to existing vm manager");
        return Ok(stream);
    }

    let lock_path = instance_dir.join(VM_MANAGER_LOCK_NAME);
    let mut lock_file = acquire_spawn_lock(&lock_path)?;
    if lock_file.is_some() {
        tracing::info!(path = %socket_path.display(), "spawning vm manager");
        spawn_manager_process(raw_args, auto_shutdown_ms, &instance_dir, config_path)?;
    } else {
        tracing::info!(
            path = %socket_path.display(),
            lock = %lock_path.display(),
            lock_pid = read_lock_pid(&lock_path).unwrap_or(0),
            "waiting for vm manager spawn lock"
        );
    }

    let start = Instant::now();
    let timeout = Duration::from_secs(10);
    loop {
        match UnixStream::connect(&socket_path) {
            Ok(stream) => {
                send_client_pid(&stream);
                tracing::info!(path = %socket_path.display(), "connected to vm manager");
                if lock_file.is_some() {
                    drop(lock_file.take());
                    let _ = fs::remove_file(&lock_path);
                }
                return Ok(stream);
            }
            Err(err) => {
                tracing::debug!(error = %err, "waiting for vm manager socket");
                if start.elapsed() > timeout {
                    if lock_file.is_some() {
                        drop(lock_file.take());
                        let _ = fs::remove_file(&lock_path);
                    }
                    return Err(format!(
                        "Timed out waiting for vm manager socket: {} ({})",
                        socket_path.display(),
                        err
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

pub fn run_manager(
    args: vm::VmArg,
    auto_shutdown_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = env::current_dir()?;
    tracing::info!(root = %project_root.display(), "vm manager starting");
    let _pid_guard = ensure_pid_file(&project_root)?;
    #[cfg(feature = "mock-vm")]
    tracing::info!("vm manager using mock executor");
    let executor: &dyn VmExecutor = {
        #[cfg(feature = "mock-vm")]
        {
            &MockVmExecutor
        }
        #[cfg(not(feature = "mock-vm"))]
        {
            &RealVmExecutor
        }
    };
    let options = {
        #[cfg(feature = "mock-vm")]
        {
            ManagerOptions {
                ensure_signed: false,
                detach: true,
                prepare_vm: false,
            }
        }
        #[cfg(not(feature = "mock-vm"))]
        {
            ManagerOptions {
                ensure_signed: true,
                detach: true,
                prepare_vm: true,
            }
        }
    };
    run_manager_with(&project_root, args, auto_shutdown_ms, executor, options)
}

fn spawn_manager_process(
    raw_args: &[std::ffi::OsString],
    auto_shutdown_ms: u64,
    instance_dir: &Path,
    config_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let mut supervisor_exe = exe.clone();
    supervisor_exe.set_file_name("vibebox-supervisor");
    let use_supervisor = supervisor_exe.exists();
    let mut cmd = if use_supervisor {
        Command::new(supervisor_exe)
    } else {
        let mut cmd = Command::new(exe);
        cmd.arg0("vibebox-supervisor");
        cmd
    };
    if raw_args.len() > 1 {
        cmd.args(&raw_args[1..]);
    }
    cmd.env("VIBEBOX_INTERNAL", "1");
    if !use_supervisor {
        cmd.env("VIBEBOX_VM_MANAGER", "1");
    }
    cmd.env("VIBEBOX_LOG_NO_COLOR", "1");
    cmd.env("VIBEBOX_AUTO_SHUTDOWN_MS", auto_shutdown_ms.to_string());
    if let Some(path) = config_path {
        cmd.env(CONFIG_PATH_ENV, path);
    }
    tracing::debug!(auto_shutdown_ms, "vm manager process spawn requested");
    let log_path = instance_dir.join(VM_MANAGER_LOG_NAME);
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .ok();
    if let Some(file) = log_file {
        let stderr = Stdio::from(file);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
    } else {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    let _child = cmd.spawn()?;
    Ok(())
}

fn ensure_pid_file(project_root: &Path) -> Result<PidFileGuard, Box<dyn std::error::Error>> {
    let instance_dir = ensure_instance_dir(project_root)?;
    let pid_path = instance_dir.join(VM_MANAGER_PID_NAME);
    let socket_path = instance_dir.join(VM_MANAGER_SOCKET_NAME);
    if let Ok(content) = fs::read_to_string(&pid_path)
        && let Ok(pid) = content.trim().parse::<u32>()
        && pid_is_alive(pid)
    {
        if is_socket_path(&socket_path) {
            return Err(format!("vm manager already running (pid {pid})").into());
        }
        tracing::warn!(
            pid,
            path = %socket_path.display(),
            "stale pid file detected with missing socket"
        );
    }
    let _ = fs::remove_file(&pid_path);
    fs::write(&pid_path, format!("{}\n", std::process::id()))?;
    let _ = fs::set_permissions(&pid_path, fs::Permissions::from_mode(0o600));
    Ok(PidFileGuard { path: pid_path })
}

fn cleanup_stale_manager(instance_dir: &Path) {
    let pid_path = instance_dir.join(VM_MANAGER_PID_NAME);
    if let Ok(content) = fs::read_to_string(&pid_path)
        && let Ok(pid) = content.trim().parse::<u32>()
        && pid_is_alive(pid)
    {
        return;
    }
    let _ = fs::remove_file(&pid_path);
}

fn inject_project_mount(
    mounts: &mut Vec<String>,
    project_root: &Path,
    ssh_user: &str,
    project_name: &str,
) {
    let guest_tilde = format!("~/{project_name}");
    let guest_home = format!("/home/{ssh_user}/{project_name}");
    let guest_base = format!("{PROJECT_GUEST_BASE}/{project_name}");
    let already_mapped = mounts.iter().any(|spec| {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 {
            return false;
        }
        let guest = parts[1];
        guest == guest_tilde || guest == guest_home || guest == guest_base
    });
    if already_mapped {
        return;
    }
    let host = project_root.display();
    mounts.insert(0, format!("{host}:{guest_tilde}:read-write"));
}

fn is_socket_path(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false)
}

fn prepare_mounts_and_links(mut args: vm::VmArg, ssh_user: &str) -> (vm::VmArg, String) {
    let mut links = Vec::new();
    let mut mounts = Vec::with_capacity(args.mounts.len());
    for spec in args.mounts {
        let (rewritten, link) = rewrite_mount_spec(&spec, ssh_user);
        if let Some(link) = link {
            links.push(link);
        }
        mounts.push(rewritten);
    }
    args.mounts = mounts;
    let script = render_home_links_script(&links, ssh_user);
    (args, script)
}

struct HomeLink {
    source: String,
    target: String,
}

fn rewrite_mount_spec(spec: &str, ssh_user: &str) -> (String, Option<HomeLink>) {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return (spec.to_string(), None);
    }
    let host = parts[0];
    let guest = parts[1];
    let mode = parts.get(2).copied();

    let home_prefix = format!("/home/{ssh_user}");
    let (rel, is_home) = if guest == "~" {
        (String::new(), true)
    } else if let Some(stripped) = guest.strip_prefix("~/") {
        (stripped.to_string(), true)
    } else if guest == home_prefix {
        (String::new(), true)
    } else if let Some(stripped) = guest.strip_prefix(&(home_prefix.clone() + "/")) {
        (stripped.to_string(), true)
    } else {
        (String::new(), false)
    };

    if !is_home {
        return (spec.to_string(), None);
    }

    let root_base = PROJECT_GUEST_BASE;
    let root_path = if rel.is_empty() {
        root_base.to_string()
    } else {
        format!("{root_base}/{rel}")
    };
    let target = if rel.is_empty() {
        home_prefix
    } else {
        format!("{home_prefix}/{rel}")
    };

    let rewritten = match mode {
        Some(mode) => format!("{host}:{root_path}:{mode}"),
        None => format!("{host}:{root_path}"),
    };

    (
        rewritten,
        Some(HomeLink {
            source: root_path,
            target,
        }),
    )
}

fn render_home_links_script(links: &[HomeLink], ssh_user: &str) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    lines.push("link_home() {".to_string());
    lines.push("  src=\"$1\"".to_string());
    lines.push("  dest=\"$2\"".to_string());
    lines.push("  if [ -L \"$dest\" ]; then".to_string());
    lines.push("    current=\"$(readlink \"$dest\" || true)\"".to_string());
    lines.push("    if [ \"$current\" != \"$src\" ]; then".to_string());
    lines.push("      rm -f \"$dest\"".to_string());
    lines.push("    fi".to_string());
    lines.push("  fi".to_string());
    lines.push("  if [ ! -e \"$dest\" ]; then".to_string());
    lines.push("    mkdir -p \"$(dirname \"$dest\")\"".to_string());
    lines.push("    ln -s \"$src\" \"$dest\"".to_string());
    lines.push("  fi".to_string());
    lines.push(format!(
        "  chown -h \"{ssh_user}:{ssh_user}\" \"$dest\" 2>/dev/null || true"
    ));
    lines.push("}".to_string());
    for link in links {
        let src = shell_escape(&link.source);
        let dest = shell_escape(&link.target);
        lines.push(format!("link_home {src} {dest}"));
    }
    lines.join("\n")
}

fn shell_escape(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

struct PidFileGuard {
    path: PathBuf,
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn detach_from_terminal() {
    unsafe {
        libc::setsid();
    }

    if let Ok(devnull) = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        let _ = unsafe { libc::dup2(devnull.as_raw_fd(), libc::STDIN_FILENO) };
        let _ = unsafe { libc::dup2(devnull.as_raw_fd(), libc::STDOUT_FILENO) };
    }
}

fn wait_for_disconnect(mut stream: UnixStream) {
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

fn send_client_pid(stream: &UnixStream) {
    let pid = std::process::id();
    let payload = format!("pid={pid}\n");
    if let Ok(mut stream) = stream.try_clone() {
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
    }
}

fn acquire_spawn_lock(lock_path: &Path) -> Result<Option<fs::File>, Box<dyn std::error::Error>> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(mut file) => {
            let pid = std::process::id();
            let _ = writeln!(file, "pid={pid}");
            Ok(Some(file))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if is_lock_stale(lock_path) {
                tracing::warn!(
                    lock = %lock_path.display(),
                    lock_pid = read_lock_pid(lock_path).unwrap_or(0),
                    "stale vm manager lock removed"
                );
                let _ = fs::remove_file(lock_path);
                return acquire_spawn_lock(lock_path);
            }
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

fn is_lock_stale(lock_path: &Path) -> bool {
    match read_lock_pid(lock_path) {
        Some(pid) => !pid_is_alive(pid),
        None => true,
    }
}

fn pid_is_alive(pid: u32) -> bool {
    let pid = pid as libc::pid_t;
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::EPERM => true,
        Some(code) if code == libc::ESRCH => false,
        _ => false,
    }
}

fn read_lock_pid(lock_path: &Path) -> Option<u32> {
    let content = fs::read_to_string(lock_path).ok()?;
    let line = content.lines().next()?;
    line.strip_prefix("pid=")?.trim().parse::<u32>().ok()
}

fn read_client_pid(stream: &UnixStream) -> Option<u32> {
    let mut stream = stream.try_clone().ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 64];
    let mut len = 0usize;
    loop {
        match stream.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => {
                len += n;
                if buf[..len].contains(&b'\n') || len == buf.len() {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => break,
            Err(_) => break,
        }
    }
    let _ = stream.set_read_timeout(None);
    if len == 0 {
        return None;
    }
    let line = String::from_utf8_lossy(&buf[..len]);
    let trimmed = line.trim();
    if let Some(value) = trimmed.strip_prefix("pid=") {
        value.parse::<u32>().ok()
    } else {
        None
    }
}

#[cfg_attr(feature = "mock-vm", allow(dead_code))]
fn spawn_manager_io(
    config: Arc<Mutex<InstanceConfig>>,
    instance_dir: PathBuf,
    output_monitor: Arc<vm::OutputMonitor>,
    vm_output_fd: std::os::unix::io::OwnedFd,
    vm_input_fd: std::os::unix::io::OwnedFd,
) -> vm::IoContext {
    let log_path = instance_dir.join(VM_ROOT_LOG_NAME);
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .ok()
        .map(|file| Arc::new(Mutex::new(file)));

    let instance_path = instance_dir.join(INSTANCE_FILENAME);
    let config_for_output = config.clone();
    let log_for_output = log_file.clone();
    let mut line_buf = String::new();

    let on_output = move |bytes: &[u8]| {
        if let Some(log) = &log_for_output
            && let Ok(mut file) = log.lock()
        {
            let _ = file.write_all(bytes);
        }

        let text = String::from_utf8_lossy(bytes);
        line_buf.push_str(&text);

        while let Some(pos) = line_buf.find('\n') {
            let mut line = line_buf[..pos].to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            line_buf.drain(..=pos);

            let cleaned = line.trim_start_matches(['\r', ' ']);
            if let Some(pos) = cleaned.find("VIBEBOX_IPV4=") {
                let ip_raw = &cleaned[(pos + "VIBEBOX_IPV4=".len())..];
                let ip = extract_ipv4(ip_raw).unwrap_or_default();
                if !ip.is_empty()
                    && let Ok(mut cfg) = config_for_output.lock()
                    && cfg.vm_ipv4.as_deref() != Some(ip.as_str())
                {
                    cfg.vm_ipv4 = Some(ip.clone());
                    let _ = write_instance_config(&instance_path, &cfg);
                }
            }
        }
    };

    vm::spawn_vm_io_with_hooks(
        output_monitor,
        vm_output_fd,
        vm_input_fd,
        vm::IoControl::new(),
        |_| false,
        on_output,
    )
}

enum ManagerEvent {
    Inc(Option<u32>),
    Dec(Option<u32>),
    VmExited(Option<String>),
}

struct ManagerOptions {
    ensure_signed: bool,
    detach: bool,
    prepare_vm: bool,
}

trait VmExecutor {
    fn run_vm(
        &self,
        args: vm::VmArg,
        extra_login_actions: Vec<LoginAction>,
        extra_shares: Vec<DirectoryShare>,
        config: Arc<Mutex<InstanceConfig>>,
        instance_dir: PathBuf,
        vm_input_tx: Arc<Mutex<Option<mpsc::Sender<VmInput>>>>,
    ) -> Result<(), Box<dyn std::error::Error>>;
}

#[cfg_attr(feature = "mock-vm", allow(dead_code))]
struct RealVmExecutor;

impl VmExecutor for RealVmExecutor {
    fn run_vm(
        &self,
        args: vm::VmArg,
        extra_login_actions: Vec<LoginAction>,
        extra_shares: Vec<DirectoryShare>,
        config: Arc<Mutex<InstanceConfig>>,
        instance_dir: PathBuf,
        vm_input_tx: Arc<Mutex<Option<mpsc::Sender<VmInput>>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        vm::run_with_args_and_extras(
            args,
            |output_monitor, vm_output_fd, vm_input_fd| {
                let io_ctx = spawn_manager_io(
                    config.clone(),
                    instance_dir.clone(),
                    output_monitor,
                    vm_output_fd,
                    vm_input_fd,
                );
                *vm_input_tx.lock().unwrap() = Some(io_ctx.input_tx.clone());
                io_ctx
            },
            extra_login_actions,
            extra_shares,
        )
    }
}

#[cfg(feature = "mock-vm")]
struct MockVmExecutor;

#[cfg(feature = "mock-vm")]
impl VmExecutor for MockVmExecutor {
    fn run_vm(
        &self,
        _args: vm::VmArg,
        _extra_login_actions: Vec<LoginAction>,
        _extra_shares: Vec<DirectoryShare>,
        _config: Arc<Mutex<InstanceConfig>>,
        _instance_dir: PathBuf,
        vm_input_tx: Arc<Mutex<Option<mpsc::Sender<VmInput>>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel::<VmInput>();
        *vm_input_tx.lock().unwrap() = Some(tx);
        tracing::info!("mock vm executor running");
        while let Ok(input) = rx.recv() {
            match input {
                VmInput::Shutdown => break,
                VmInput::Bytes(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if text.contains("systemctl poweroff") {
                        break;
                    }
                }
            }
        }
        tracing::info!("mock vm executor exiting");
        Ok(())
    }
}

fn run_manager_with(
    project_root: &Path,
    mut args: vm::VmArg,
    auto_shutdown_ms: u64,
    executor: &dyn VmExecutor,
    options: ManagerOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    if options.ensure_signed {
        let _had_skip = env::var("VIBEBOX_SKIP_CODESIGN").ok();
        unsafe {
            env::remove_var("VIBEBOX_SKIP_CODESIGN");
        }
        vm::ensure_signed();
        unsafe {
            env::set_var("VIBEBOX_SKIP_CODESIGN", "1");
        }
    }
    if options.detach {
        detach_from_terminal();
    }

    let project_name = project_root
        .file_name()
        .ok_or("Project directory has no name")?
        .to_string_lossy()
        .into_owned();
    let instance_dir = ensure_instance_dir(project_root)?;
    if options.prepare_vm {
        let _ = ensure_ssh_keypair(&instance_dir)?;
    }

    let mut config = load_or_create_instance_config(&instance_dir)?;
    if config.vm_ipv4.is_some() {
        config.vm_ipv4 = None;
        write_instance_config(&instance_dir.join(INSTANCE_FILENAME), &config)?;
    }
    let config = Arc::new(Mutex::new(config));
    let ssh_user = config
        .lock()
        .map(|cfg| cfg.ssh_user_display())
        .unwrap_or_else(|_| DEFAULT_SSH_USER.to_string());
    if !args.no_default_mounts {
        inject_project_mount(&mut args.mounts, project_root, &ssh_user, &project_name);
    }
    let (args, home_links_script) = prepare_mounts_and_links(args, &ssh_user);

    let project_guest_dir = format!("{PROJECT_GUEST_BASE}/{project_name}");
    let ssh_guest_dir = format!("/root/{}", GLOBAL_DIR_NAME);
    let extra_shares = vec![DirectoryShare::new(
        instance_dir.clone(),
        ssh_guest_dir.clone().into(),
        true,
    )?];
    let extra_login_actions = build_ssh_login_actions(
        &config,
        &project_name,
        &project_guest_dir,
        ssh_guest_dir.as_str(),
        "ssh_key",
        &home_links_script,
    );

    let socket_path = instance_dir.join(VM_MANAGER_SOCKET_NAME);
    if let Ok(stream) = UnixStream::connect(&socket_path) {
        drop(stream);
        return Ok(());
    }
    if socket_path.exists() {
        let _ = fs::remove_file(&socket_path);
    }

    let listener = UnixListener::bind(&socket_path)?;
    let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600));
    tracing::info!(path = %socket_path.display(), "vm manager socket bound");

    let (event_tx, event_rx) = mpsc::channel::<ManagerEvent>();
    let event_tx_accept = event_tx.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let event_tx_conn = event_tx_accept.clone();
                    thread::spawn(move || {
                        let pid = read_client_pid(&stream);
                        let _ = event_tx_conn.send(ManagerEvent::Inc(pid));
                        wait_for_disconnect(stream);
                        let _ = event_tx_conn.send(ManagerEvent::Dec(pid));
                    });
                }
                Err(_) => break,
            }
        }
    });

    let vm_input_tx: Arc<Mutex<Option<mpsc::Sender<VmInput>>>> = Arc::new(Mutex::new(None));
    let vm_input_for_loop = vm_input_tx.clone();
    let event_loop_handle =
        thread::spawn(move || manager_event_loop(event_rx, vm_input_for_loop, auto_shutdown_ms));

    tracing::info!("vm manager launching vm");
    let vm_result = executor.run_vm(
        args,
        extra_login_actions,
        extra_shares,
        config.clone(),
        instance_dir.clone(),
        vm_input_tx.clone(),
    );
    tracing::info!("vm manager vm run completed");
    let vm_err = vm_result.err().map(|e| e.to_string());
    let _ = event_tx.send(ManagerEvent::VmExited(vm_err.clone()));
    let event_loop_result: Result<(), String> = event_loop_handle
        .join()
        .unwrap_or_else(|_| Err("vm manager event loop panicked".into()))
        .map_err(|err| err.to_string());
    let _ = fs::remove_file(&socket_path);
    if let Err(err) = &event_loop_result {
        tracing::error!(error = %err, "vm manager exiting due to event loop error");
        return Err(err.to_string().into());
    }
    if let Some(err) = vm_err {
        tracing::error!(error = %err, "vm manager exiting due to vm error");
        return Err(err.into());
    }
    tracing::info!("vm manager exiting");
    Ok(event_loop_result?)
}

fn manager_event_loop(
    event_rx: mpsc::Receiver<ManagerEvent>,
    vm_input_tx: Arc<Mutex<Option<mpsc::Sender<VmInput>>>>,
    auto_shutdown_ms: u64,
) -> Result<(), String> {
    let mut ref_count: usize = 0;
    let mut shutdown_deadline: Option<Instant> = None;
    let mut shutdown_sent = false;
    let mut hard_deadline: Option<Instant> = None;
    let grace = Duration::from_millis(auto_shutdown_ms.max(1));
    let hard_timeout = Duration::from_millis(HARD_SHUTDOWN_TIMEOUT_MS);

    loop {
        let timeout = match (shutdown_deadline, hard_deadline) {
            (Some(shutdown), Some(hard)) => {
                let next = if shutdown <= hard { shutdown } else { hard };
                next.saturating_duration_since(Instant::now())
            }
            (Some(shutdown), None) => shutdown.saturating_duration_since(Instant::now()),
            (None, Some(hard)) => hard.saturating_duration_since(Instant::now()),
            (None, None) => Duration::from_secs(1),
        };

        match event_rx.recv_timeout(timeout) {
            Ok(ManagerEvent::Inc(pid)) => {
                ref_count = ref_count.saturating_add(1);
                tracing::info!(
                    ref_count,
                    pid = pid.unwrap_or(0),
                    pid_known = pid.is_some(),
                    "vm manager refcount increment"
                );
                shutdown_deadline = None;
                shutdown_sent = false;
                hard_deadline = None;
            }
            Ok(ManagerEvent::Dec(pid)) => {
                ref_count = ref_count.saturating_sub(1);
                tracing::info!(
                    ref_count,
                    pid = pid.unwrap_or(0),
                    pid_known = pid.is_some(),
                    "vm manager refcount decrement"
                );
                if ref_count == 0 {
                    shutdown_deadline = Some(Instant::now() + grace);
                    tracing::info!(grace_ms = auto_shutdown_ms, "shutdown scheduled");
                }
            }
            Ok(ManagerEvent::VmExited(err)) => {
                if let Some(err) = err {
                    tracing::error!(error = %err, "vm exited with an error");
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(deadline) = shutdown_deadline
                    && Instant::now() >= deadline
                    && !shutdown_sent
                {
                    if hard_deadline.is_none() {
                        hard_deadline = Some(Instant::now() + hard_timeout);
                    }
                    let mut sent = false;
                    if let Some(tx) = vm_input_tx.lock().unwrap().clone() {
                        if tx
                            .send(VmInput::Bytes(b"systemctl poweroff\n".to_vec()))
                            .is_ok()
                        {
                            sent = true;
                        } else {
                            tracing::warn!("shutdown command failed to send");
                        }
                    } else {
                        tracing::warn!("shutdown command deferred; vm input not ready");
                    }
                    if sent {
                        tracing::info!("shutdown command sent");
                        shutdown_sent = true;
                        shutdown_deadline = None;
                        hard_deadline = Some(Instant::now() + hard_timeout);
                    } else {
                        shutdown_deadline =
                            Some(Instant::now() + Duration::from_millis(SHUTDOWN_RETRY_MS));
                    }
                }
                if ref_count == 0
                    && let Some(deadline) = hard_deadline
                    && Instant::now() >= deadline
                {
                    if shutdown_sent {
                        tracing::warn!(
                            timeout_ms = HARD_SHUTDOWN_TIMEOUT_MS,
                            "force exiting: VM did not stop after shutdown timeout"
                        );
                    } else {
                        tracing::warn!(
                            timeout_ms = HARD_SHUTDOWN_TIMEOUT_MS,
                            "force exiting: VM input not ready after shutdown timeout"
                        );
                    }
                    force_exit("vm manager forced exit");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn manager_powers_off_after_grace_when_no_refs() {
        let _temp = tempfile::Builder::new()
            .prefix("vb")
            .tempdir_in("/tmp")
            .expect("tempdir");

        let (event_tx, event_rx) = mpsc::channel::<ManagerEvent>();
        let (vm_tx, vm_rx) = mpsc::channel::<VmInput>();
        let vm_input_tx = Arc::new(Mutex::new(Some(vm_tx)));

        let manager_thread = thread::spawn(move || {
            manager_event_loop(event_rx, vm_input_tx, 50).expect("event loop");
        });

        event_tx.send(ManagerEvent::Inc(None)).unwrap();
        assert!(vm_rx.recv_timeout(Duration::from_millis(100)).is_err());

        event_tx.send(ManagerEvent::Dec(None)).unwrap();
        let msg = vm_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("poweroff");
        match msg {
            VmInput::Bytes(data) => {
                assert_eq!(data, b"systemctl poweroff\n");
            }
            _ => panic!("unexpected vm input"),
        }
        let _ = event_tx.send(ManagerEvent::VmExited(None));
        let _ = manager_thread.join();
    }

    #[test]
    fn manager_force_exits_when_vm_input_never_ready() {
        let (event_tx, event_rx) = mpsc::channel::<ManagerEvent>();
        let vm_input_tx = Arc::new(Mutex::new(None));

        let manager_thread = thread::spawn(move || {
            let _ = manager_event_loop(event_rx, vm_input_tx, 10);
        });

        event_tx.send(ManagerEvent::Inc(None)).unwrap();
        event_tx.send(ManagerEvent::Dec(None)).unwrap();

        let join_result = manager_thread.join();
        assert!(
            join_result.is_err(),
            "expected manager to force-exit when vm input never becomes ready"
        );
    }

    #[test]
    fn manager_sends_shutdown_after_vm_input_becomes_ready() {
        let (event_tx, event_rx) = mpsc::channel::<ManagerEvent>();
        let (vm_tx, vm_rx) = mpsc::channel::<VmInput>();
        let vm_input_tx = Arc::new(Mutex::new(None));
        let vm_input_for_thread = vm_input_tx.clone();

        let manager_thread = thread::spawn(move || {
            manager_event_loop(event_rx, vm_input_for_thread, 10).expect("event loop");
        });

        event_tx.send(ManagerEvent::Inc(None)).unwrap();
        event_tx.send(ManagerEvent::Dec(None)).unwrap();

        thread::sleep(Duration::from_millis(100));
        *vm_input_tx.lock().unwrap() = Some(vm_tx);

        let msg = vm_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("poweroff");
        match msg {
            VmInput::Bytes(data) => {
                assert_eq!(data, b"systemctl poweroff\n");
            }
            _ => panic!("unexpected vm input"),
        }
        let _ = event_tx.send(ManagerEvent::VmExited(None));
        let _ = manager_thread.join();
    }
}
