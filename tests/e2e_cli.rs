use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

#[test]
fn cli_version_shows_binary_name() {
    let output = cargo_bin_cmd!("vibebox").arg("--version").output().unwrap();
    print_output("e2e_cli", &output);
    assert!(
        output.status.success(),
        "expected success, got status: {}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("vibebox"),
        "expected --version output to contain 'vibebox', got: {}",
        stdout
    );
}

#[test]
fn list_reports_no_sessions_when_empty() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let output = cargo_bin_cmd!("vibebox")
        .current_dir(&project)
        .env("HOME", &home)
        .arg("list")
        .output()
        .unwrap();
    print_output("e2e_cli", &output);
    assert!(
        output.status.success(),
        "expected success, got status: {}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No sessions were found."),
        "expected empty sessions message, got: {}",
        stdout
    );
}

fn print_output(prefix: &str, output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        println!("[{}] {}", prefix, line);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        eprintln!("[{}] {}", prefix, line);
    }
}
