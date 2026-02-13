use std::env;
use std::path::{Path, PathBuf};

pub fn relative_to_home(directory: &Path) -> String {
    let Ok(home) = env::var("HOME") else {
        return directory.display().to_string();
    };
    let home_path = PathBuf::from(home);
    if let Ok(stripped) = directory.strip_prefix(&home_path) {
        if stripped.components().next().is_none() {
            return "~".to_string();
        }
        return format!("~/{}", stripped.display());
    }
    directory.display().to_string()
}

pub fn pid_is_alive(pid: u32) -> bool {
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
