#![cfg(all(feature = "mock-vm", target_os = "macos"))]

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[test]
fn mock_vm_allows_refcount_concurrency() {
    let temp = TempDir::new().unwrap();
    let mut supervisor = spawn_supervisor(&temp, 0, 1200, "e2e_vm_mock".to_string());

    supervisor.clients = connect_clients(
        &supervisor.socket_path,
        12,
        Duration::from_secs(2),
        true,
        "e2e_vm_mock",
    );
    log_line("e2e_vm_mock", "connected 12 clients");

    assert_manager_alive_for(
        &mut supervisor.child,
        Duration::from_millis(900),
        "vm manager exited while clients active",
    );

    let remaining = supervisor.clients.split_off(6);
    supervisor.clients = remaining;
    log_line("e2e_vm_mock", "dropped 6 clients");
    assert_manager_alive_for(
        &mut supervisor.child,
        Duration::from_millis(900),
        "vm manager exited while clients active",
    );

    supervisor.clients.clear();
    log_line("e2e_vm_mock", "dropped remaining clients");
    wait_for_exit(&mut supervisor.child, Duration::from_secs(10));
    let status = supervisor.child.wait().unwrap();
    assert!(status.success(), "vm manager exited with {status}");
}

#[test]
fn mock_vm_many_managers_many_clients() {
    let temp = TempDir::new().unwrap();
    let mut supervisors = Vec::new();

    for idx in 0..3 {
        supervisors.push(spawn_supervisor(
            &temp,
            idx + 1,
            1400,
            format!("e2e_vm_mock_{idx}"),
        ));
    }

    for supervisor in &mut supervisors {
        supervisor.clients = connect_clients(
            &supervisor.socket_path,
            8,
            Duration::from_secs(2),
            true,
            "e2e_vm_mock",
        );
    }
    log_line("e2e_vm_mock", "connected 8 clients per manager");

    for supervisor in &mut supervisors {
        assert_manager_alive_for(
            &mut supervisor.child,
            Duration::from_millis(900),
            "manager exited while clients active",
        );
    }

    supervisors[0].clients.clear();
    log_line("e2e_vm_mock", "dropped all clients for manager 0");
    wait_for_exit(&mut supervisors[0].child, Duration::from_secs(10));
    let status = supervisors[0].child.wait().unwrap();
    assert!(status.success(), "manager 0 exited with {status}");

    for supervisor in supervisors.iter_mut().skip(1) {
        assert_manager_alive_for(
            &mut supervisor.child,
            Duration::from_millis(900),
            "another manager exited early",
        );
    }

    for supervisor in supervisors.iter_mut().skip(1) {
        supervisor.clients.clear();
    }
    log_line("e2e_vm_mock", "dropped remaining clients");
    for supervisor in supervisors.iter_mut().skip(1) {
        wait_for_exit(&mut supervisor.child, Duration::from_secs(10));
        let status = supervisor.child.wait().unwrap();
        assert!(status.success(), "manager exited with {status}");
    }
}

#[test]
fn mock_vm_monkey_processes() {
    let temp = TempDir::new().unwrap();
    let mut rng = Lcg::new(0x5eed_f00d_dead_beef);
    let mut supervisors = Vec::new();
    let mut next_id = 0usize;
    let max_supervisors = 4usize;
    let steps = 25usize;

    supervisors.push(spawn_supervisor(
        &temp,
        next_id,
        1200,
        format!("e2e_vm_monkey_{next_id}"),
    ));
    next_id += 1;

    for step in 0..steps {
        prune_exited_supervisors(&mut supervisors, "e2e_vm_monkey");
        let roll = rng.gen_range(100);
        log_line(
            "e2e_vm_monkey",
            &format!("step {step} roll={roll} supervisors={}", supervisors.len()),
        );
        if roll < 20 && supervisors.len() < max_supervisors {
            supervisors.push(spawn_supervisor(
                &temp,
                next_id,
                1200,
                format!("e2e_vm_monkey_{next_id}"),
            ));
            log_line("e2e_vm_monkey", &format!("spawned supervisor {next_id}"));
            next_id += 1;
        } else if roll < 45 && !supervisors.is_empty() {
            let idx = rng.gen_range(supervisors.len());
            let mut supervisor = supervisors.swap_remove(idx);
            log_line(
                "e2e_vm_monkey",
                &format!("killing supervisor {}", supervisor.label),
            );
            kill_supervisor(&mut supervisor, Duration::from_secs(5));
        } else if roll < 80 && !supervisors.is_empty() {
            let idx = rng.gen_range(supervisors.len());
            let burst = 1 + rng.gen_range(3);
            let new_clients = connect_clients(
                &supervisors[idx].socket_path,
                burst,
                Duration::from_secs(1),
                false,
                "e2e_vm_monkey",
            );
            supervisors[idx].clients.extend(new_clients);
            log_line(
                "e2e_vm_monkey",
                &format!("connected {burst} clients to {}", supervisors[idx].label),
            );
        } else if !supervisors.is_empty() {
            let idx = rng.gen_range(supervisors.len());
            if !supervisors[idx].clients.is_empty() {
                let len = supervisors[idx].clients.len();
                let drop_count = 1 + rng.gen_range(len);
                supervisors[idx].clients.drain(0..drop_count.min(len));
                log_line(
                    "e2e_vm_monkey",
                    &format!(
                        "dropped {drop_count} clients from {}",
                        supervisors[idx].label
                    ),
                );
            }
        }
        thread::sleep(Duration::from_millis(200));
    }

    log_line("e2e_vm_monkey", "final cleanup");
    for supervisor in supervisors.iter_mut() {
        shutdown_supervisor(supervisor, Duration::from_secs(10));
    }
}

#[test]
fn mock_vm_exits_without_clients() {
    let temp = TempDir::new().unwrap();
    let mut supervisor = spawn_supervisor(&temp, 99, 300, "e2e_vm_no_clients".to_string());
    wait_for_exit(&mut supervisor.child, Duration::from_secs(5));
    let status = supervisor.child.wait().unwrap();
    assert!(status.success(), "vm manager exited with {status}");
}

#[test]
fn mock_vm_reconnect_resets_shutdown() {
    let temp = TempDir::new().unwrap();
    let mut supervisor = spawn_supervisor(&temp, 100, 800, "e2e_vm_reconnect".to_string());

    supervisor.clients = connect_clients(
        &supervisor.socket_path,
        1,
        Duration::from_secs(2),
        true,
        "e2e_vm_reconnect",
    );
    supervisor.clients.clear();
    thread::sleep(Duration::from_millis(400));

    supervisor.clients = connect_clients(
        &supervisor.socket_path,
        1,
        Duration::from_secs(2),
        true,
        "e2e_vm_reconnect",
    );
    assert_manager_alive_for(
        &mut supervisor.child,
        Duration::from_millis(600),
        "vm manager exited despite reconnect",
    );

    supervisor.clients.clear();
    wait_for_exit(&mut supervisor.child, Duration::from_secs(10));
    let status = supervisor.child.wait().unwrap();
    assert!(status.success(), "vm manager exited with {status}");
}

struct Supervisor {
    child: Child,
    socket_path: PathBuf,
    clients: Vec<UnixStream>,
    label: String,
}

fn write_config(project: &Path, auto_shutdown_ms: u64) {
    let config = format!(
        r#"[box]
cpu_count = 2
ram_mb = 2048
disk_gb = 5
mounts = []

[supervisor]
auto_shutdown_ms = {auto_shutdown_ms}
"#
    );
    fs::write(project.join("vibebox.toml"), config).unwrap();
}

fn spawn_supervisor(
    temp: &TempDir,
    idx: usize,
    auto_shutdown_ms: u64,
    label: String,
) -> Supervisor {
    let home = temp.path().join(format!("home-{idx}"));
    let cache_home = temp.path().join(format!("cache-{idx}"));
    let project = temp.path().join(format!("project-{idx}"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache_home).unwrap();
    fs::create_dir_all(&project).unwrap();
    write_config(&project, auto_shutdown_ms);

    let mut child = Command::new(assert_cmd::cargo_bin!("vibebox-supervisor"))
        .current_dir(&project)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache_home)
        .env("VIBEBOX_INTERNAL", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(stdout) = child.stdout.take() {
        spawn_prefix_reader(label.clone(), "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_prefix_reader(label.clone(), "stderr", stderr);
    }

    let socket_path = project.join(".vibebox").join("vm.sock");
    wait_for_socket(&socket_path, Duration::from_secs(10));

    Supervisor {
        child,
        socket_path,
        clients: Vec::new(),
        label,
    }
}

fn connect_clients(
    socket_path: &Path,
    count: usize,
    timeout: Duration,
    require_all: bool,
    label: &str,
) -> Vec<UnixStream> {
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count {
        let path = socket_path.to_path_buf();
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let stream = connect_client_with_retry(&path, timeout);
            let _ = tx.send(stream);
        }));
    }
    drop(tx);
    let mut clients = Vec::with_capacity(count);
    for stream in rx.into_iter().flatten() {
        clients.push(stream);
    }
    for handle in handles {
        handle.join().unwrap();
    }
    if require_all && clients.len() != count {
        panic!(
            "client count mismatch: expected {count} got {}",
            clients.len()
        );
    }
    if !require_all && clients.len() != count {
        log_line(
            label,
            &format!("connected {} of {count} clients", clients.len()),
        );
    }
    clients
}

fn connect_client_with_retry(path: &Path, timeout: Duration) -> Option<UnixStream> {
    let start = Instant::now();
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Some(stream),
            Err(_) => {
                if start.elapsed() > timeout {
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let start = Instant::now();
    loop {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for socket {}", path.display());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn assert_manager_alive(child: &mut Child, message: &str) {
    assert!(child.try_wait().unwrap().is_none(), "{message}");
}

fn assert_manager_alive_for(child: &mut Child, duration: Duration, message: &str) {
    let start = Instant::now();
    while start.elapsed() < duration {
        assert_manager_alive(child, message);
        thread::sleep(Duration::from_millis(100));
    }
}

fn kill_supervisor(supervisor: &mut Supervisor, timeout: Duration) {
    supervisor.clients.clear();
    let _ = supervisor.child.kill();
    wait_for_exit(&mut supervisor.child, timeout);
    let _ = supervisor.child.wait();
}

fn prune_exited_supervisors(supervisors: &mut Vec<Supervisor>, label: &str) {
    supervisors.retain_mut(|supervisor| {
        if supervisor.child.try_wait().unwrap().is_some() {
            log_line(label, &format!("removed exited {}", supervisor.label));
            false
        } else {
            true
        }
    });
}

fn shutdown_supervisor(supervisor: &mut Supervisor, timeout: Duration) {
    supervisor.clients.clear();
    wait_for_exit(&mut supervisor.child, timeout);
    if supervisor.child.try_wait().unwrap().is_none() {
        let _ = supervisor.child.kill();
        let _ = supervisor.child.wait();
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) {
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for mock vm supervisor exit");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn spawn_prefix_reader(label: String, stream: &'static str, reader: impl Read + Send + 'static) {
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            match line {
                Ok(line) => println!("[{}][{}] {}", label, stream, line),
                Err(err) => {
                    eprintln!("[{}][{}] read error: {}", label, stream, err);
                    break;
                }
            }
        }
    });
}

fn log_line(prefix: &str, message: &str) {
    println!("[{}] {}", prefix, message);
}

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.state >> 32) as u32
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        (self.next_u32() as usize) % upper
    }
}
