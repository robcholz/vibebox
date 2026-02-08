use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

#[test]
fn cli_version_shows_binary_name() {
    Command::cargo_bin("vibebox")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("vibebox"));
}

#[test]
fn list_reports_no_sessions_when_empty() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    Command::cargo_bin("vibebox")
        .unwrap()
        .current_dir(&project)
        .env("HOME", &home)
        .arg("list")
        .assert()
        .success()
        .stdout(contains("No sessions were found."));
}
