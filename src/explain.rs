use crate::utils::relative_to_home;
use crate::{config, instance, session_manager, tui};
use anyhow::{Context, Result, bail};
use std::{
    env,
    path::{Path, PathBuf},
};
use tracing::warn;

pub fn build_mount_rows(cwd: &Path, config: &config::Config) -> Result<Vec<tui::MountListRow>> {
    let mut rows = Vec::new();
    rows.extend(default_mounts(cwd)?);
    let guest_home = resolve_guest_home(cwd);
    for spec in &config.box_cfg.mounts {
        rows.push(parse_mount_spec(cwd, spec, false, &guest_home)?);
    }
    Ok(rows)
}

pub fn build_network_rows(cwd: &Path) -> Result<Vec<tui::NetworkListRow>> {
    let instance_dir = cwd.join(session_manager::INSTANCE_DIR_NAME);
    let mut vm_ip = "-".to_string();
    if let Ok(Some(ip)) = instance::read_instance_vm_ip(&instance_dir) {
        vm_ip = ip;
    }
    let host_to_vm = if vm_ip == "-" {
        "ssh: <pending>:22".to_string()
    } else {
        format!("ssh: {vm_ip}:22")
    };
    let row = tui::NetworkListRow {
        network_type: "NAT".to_string(),
        vm_ip: vm_ip.clone(),
        host_to_vm,
        vm_to_host: "none".to_string(),
    };
    Ok(vec![row])
}

fn default_mounts(cwd: &Path) -> Result<Vec<tui::MountListRow>> {
    let project_name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| "failed to get project name")?;
    let project_guest = format!("~/{project_name}");
    let project_host = relative_to_home(cwd);
    let mut rows = vec![tui::MountListRow {
        host: project_host,
        guest: project_guest,
        mode: "read-write".to_string(),
        default_mount: "yes".to_string(),
    }];

    let home = env::var("HOME")
        .map(PathBuf::from)
        .with_context(|| "failed to get home directory")?;
    let cache_home = env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            warn!("failed to get XDG_CACHE_HOME, falling back to ~/.cache");
            home.join(".cache")
        });
    let cache_dir = cache_home.join(session_manager::GLOBAL_CACHE_DIR_NAME);
    let guest_mise_cache = cache_dir.join(".guest-mise-cache");
    rows.push(tui::MountListRow {
        host: relative_to_home(&guest_mise_cache),
        guest: "/root/.local/share/mise".to_string(),
        mode: "read-write".to_string(),
        default_mount: "yes".to_string(),
    });
    Ok(rows)
}

fn parse_mount_spec(
    cwd: &Path,
    spec: &str,
    default_mount: bool,
    guest_home: &str,
) -> Result<tui::MountListRow> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        bail!["invalid mount spec: {spec}"];
    }
    let host_part = parts[0];
    let guest_part = parts[1];
    let mode = if parts.len() == 3 {
        match parts[2] {
            "read-only" => "read-only",
            "read-write" => "read-write",
            other => {
                bail![format!(
                    "invalid mount mode '{}'; expected read-only or read-write",
                    other
                )];
            }
        }
    } else {
        "read-write"
    };

    let host_display = display_host_spec(cwd, host_part);
    let guest_display = resolve_guest_display(guest_part, guest_home);
    Ok(tui::MountListRow {
        host: host_display,
        guest: guest_display,
        mode: mode.to_string(),
        default_mount: if default_mount { "yes" } else { "no" }.to_string(),
    })
}

fn display_host_spec(cwd: &Path, host: &str) -> String {
    if host == "~" || host.starts_with("~/") {
        return host.to_string();
    }
    let host_path = PathBuf::from(host);
    if host_path.is_absolute() {
        return relative_to_home(&host_path);
    }
    let candidate = cwd.join(&host_path);
    if candidate.is_absolute() {
        relative_to_home(&candidate)
    } else {
        host.to_string()
    }
}

fn resolve_guest_home(cwd: &Path) -> String {
    let instance_dir = cwd.join(session_manager::INSTANCE_DIR_NAME);
    format!("/home/{}", instance::read_instance_ssh_user(&instance_dir))
}

fn resolve_guest_display(guest: &str, guest_home: &str) -> String {
    if guest == "~" {
        return "~".to_string();
    }
    if let Some(stripped) = guest.strip_prefix("~/") {
        return format!("~/{stripped}");
    }
    if Path::new(guest).is_absolute() {
        if let Ok(stripped) = Path::new(guest).strip_prefix(guest_home) {
            if stripped.components().next().is_none() {
                "~".to_string()
            } else {
                format!("~/{}", stripped.display())
            }
        } else {
            guest.to_string()
        }
    } else {
        format!("/root/{guest}")
    }
}
