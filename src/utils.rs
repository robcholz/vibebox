use std::{
    env, fs,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmManagerLiveness {
    RunningWithSocket { pid: u32 },
    RunningWithoutSocket { pid: u32 },
    NotRunningOrMissing,
}

pub fn vm_manager_liveness(pid_path: &Path, socket_path: &Path) -> VmManagerLiveness {
    let Ok(content) = fs::read_to_string(pid_path) else {
        return VmManagerLiveness::NotRunningOrMissing;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return VmManagerLiveness::NotRunningOrMissing;
    };
    if !pid_is_alive(pid) {
        return VmManagerLiveness::NotRunningOrMissing;
    }
    let has_socket = fs::metadata(socket_path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false);
    if has_socket {
        VmManagerLiveness::RunningWithSocket { pid }
    } else {
        VmManagerLiveness::RunningWithoutSocket { pid }
    }
}
