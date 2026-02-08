use assert_cmd::cargo::cargo_bin_cmd;
use predicates::str::contains;
use tempfile::TempDir;

#[test]
fn cli_version_shows_binary_name() {
    cargo_bin_cmd!("vibebox")
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

    cargo_bin_cmd!("vibebox")
        .current_dir(&project)
        .env("HOME", &home)
        .arg("list")
        .assert()
        .success()
        .stdout(contains("No sessions were found."));
}
