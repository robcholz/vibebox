#![cfg(all(feature = "mock-vm", target_os = "macos"))]

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[test]
fn mock_vm_allows_refcount_concurrency() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let cache_home = temp.path().join("cache");
    let project = temp.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache_home).unwrap();
    fs::create_dir_all(&project).unwrap();

    write_config(&project, 1000);

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
        spawn_prefix_reader("e2e_vm_mock", "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_prefix_reader("e2e_vm_mock", "stderr", stderr);
    }

    let socket_path = project.join(".vibebox").join("vm.sock");
    wait_for_socket(&socket_path, Duration::from_secs(10));

    let conn1 = UnixStream::connect(&socket_path).unwrap();
    let conn2 = UnixStream::connect(&socket_path).unwrap();
    log_line("e2e_vm_mock", "connected 2 clients");

    thread::sleep(Duration::from_millis(1800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "vm manager exited while connections were active"
    );

    drop(conn1);
    log_line("e2e_vm_mock", "dropped client 1");
    thread::sleep(Duration::from_millis(1800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "vm manager exited while a connection was still active"
    );

    drop(conn2);
    log_line("e2e_vm_mock", "dropped client 2");
    wait_for_exit(&mut child, Duration::from_secs(10));
    let status = child.wait().unwrap();
    assert!(status.success(), "vm manager exited with {status}");
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

fn spawn_prefix_reader(
    label: &'static str,
    stream: &'static str,
    reader: impl Read + Send + 'static,
) {
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
