use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn vm_boots_and_runs_command() {
    if std::env::var("VIBEBOX_E2E_VM").as_deref() != Ok("1") {
        eprintln!("skipping: set VIBEBOX_E2E_VM=1 to run this test");
        return;
    }

    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let cache_home = temp.path().join("cache");
    let project = temp.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache_home).unwrap();
    fs::create_dir_all(&project).unwrap();

    write_config(&project);

    let child = Command::new(assert_cmd::cargo_bin!("vibebox-supervisor"))
        .current_dir(&project)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache_home)
        .env("VIBEBOX_INTERNAL", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _child_guard = ChildGuard::new(child);

    let socket_path = project.join(".vibebox").join("vm.sock");
    let _socket_guard = wait_for_socket(&socket_path, Duration::from_secs(30));

    let instance_path = project.join(".vibebox").join("instance.toml");
    let (ip, user) = wait_for_vm_ip(&instance_path, Duration::from_secs(180));

    let ssh_key = project.join(".vibebox").join("ssh_key");
    wait_for_file(&ssh_key, Duration::from_secs(30));

    let output = wait_for_ssh_command(&ssh_key, &user, &ip, Duration::from_secs(90));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Linux"),
        "expected ssh command output to contain 'Linux', got: {}",
        stdout
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
#[ignore]
fn vm_boots_and_runs_command() {
    eprintln!("skipping: vm e2e test requires macOS virtualization");
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct SocketGuard {
    _path: PathBuf,
    _stream: std::os::unix::net::UnixStream,
}

fn write_config(project: &Path) {
    let config = r#"[box]
cpu_count = 2
ram_mb = 2048
disk_gb = 5
mounts = []

[supervisor]
auto_shutdown_ms = 120000
"#;
    fs::write(project.join("vibebox.toml"), config).unwrap();
}

fn wait_for_socket(path: &Path, timeout: Duration) -> SocketGuard {
    let start = Instant::now();
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(path) {
            return SocketGuard {
                _path: path.to_path_buf(),
                _stream: stream,
            };
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for vm manager socket at {}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_vm_ip(instance_path: &Path, timeout: Duration) -> (String, String) {
    let start = Instant::now();
    loop {
        if let Ok(raw) = fs::read_to_string(instance_path)
            && let Ok(value) = toml::from_str::<toml::Value>(&raw)
        {
            let ip = value
                .get("vm_ipv4")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(ip) = ip {
                let user = value
                    .get("ssh_user")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vibecoder".to_string());
                return (ip, user);
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for vm_ipv4 in {}",
                instance_path.display()
            );
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let start = Instant::now();
    loop {
        if path.exists() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for file {}", path.display());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_ssh_command(
    ssh_key: &Path,
    user: &str,
    ip: &str,
    timeout: Duration,
) -> std::process::Output {
    let start = Instant::now();
    loop {
        let output = Command::new("ssh")
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
                &format!("{user}@{ip}"),
                "uname -s",
            ])
            .output()
            .unwrap();
        if output.status.success() {
            return output;
        }
        if start.elapsed() > timeout {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("ssh command failed and timed out: {}", stderr);
        }
        thread::sleep(Duration::from_millis(1000));
    }
}
