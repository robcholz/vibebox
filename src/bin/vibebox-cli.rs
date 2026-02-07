use std::{
    env,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::Parser;
use color_eyre::Result;
use dialoguer::Confirm;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing_subscriber::EnvFilter;

use vibebox::tui::{AppState, VmInfo};
use vibebox::{SessionManager, commands, config, instance, session_manager, tui, vm, vm_manager};

#[derive(Debug, Parser)]
#[command(name = "vibebox", version, about = "Vibebox CLI")]
struct Cli {
    /// Path to vibebox.toml (relative to the current directory)
    #[arg(short = 'c', long = "config", value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// List all sessions
    List,
    /// Delete the current project's .vibebox directory
    Clean,
    /// Explain mounts and mappings
    Explain,
}

fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let cli = Cli::parse();
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    tracing::info!(cwd = %cwd.display(), "starting vibebox cli");
    if let Some(command) = cli.command {
        return handle_command(command, &cwd, cli.config.as_deref());
    }

    let config_override = cli.config.clone();
    let raw_args: Vec<OsString> = env::args_os().collect();
    let config = config::load_config_with_path(&cwd, config_override.as_deref());

    if env::var("VIBEBOX_VM_MANAGER").as_deref() == Ok("1") {
        tracing::info!("starting vm manager mode");
        let args = vm::VmArg {
            cpu_count: config.box_cfg.cpu_count,
            ram_bytes: config.box_cfg.ram_mb.saturating_mul(1024 * 1024),
            no_default_mounts: false,
            mounts: config.box_cfg.mounts.clone(),
        };
        let auto_shutdown_ms = config.supervisor.auto_shutdown_ms;
        tracing::info!(auto_shutdown_ms, "vm manager config");
        if let Err(err) = vm_manager::run_manager(args, auto_shutdown_ms) {
            tracing::error!(error = %err, "vm manager exited");
            return Err(color_eyre::eyre::eyre!(err.to_string()));
        }
        return Ok(());
    }

    vm::ensure_signed();

    let vm_args = vm::VmArg {
        cpu_count: config.box_cfg.cpu_count,
        ram_bytes: config.box_cfg.ram_mb.saturating_mul(1024 * 1024),
        no_default_mounts: false,
        mounts: config.box_cfg.mounts.clone(),
    };

    let vm_info = VmInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        max_memory_mb: vm_args.ram_bytes / (1024 * 1024),
        cpu_cores: vm_args.cpu_count,
    };
    let auto_shutdown_ms = config.supervisor.auto_shutdown_ms;
    if let Ok(manager) = SessionManager::new() {
        if let Err(err) = manager.update_global_sessions(&cwd) {
            tracing::warn!(error = %err, "failed to update a global session list");
        }
    } else {
        tracing::warn!("failed to initialize session manager");
    }
    let commands = commands::build_commands();
    let app = Arc::new(Mutex::new(AppState::new(cwd.clone(), vm_info, commands)));

    {
        let mut locked = app.lock().expect("app state poisoned");
        tui::render_tui_once(&mut locked)?;
    }
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout)?;
        stdout.flush()?;
    }

    tracing::info!(auto_shutdown_ms, "auto shutdown config");
    let manager_conn =
        vm_manager::ensure_manager(&raw_args, auto_shutdown_ms, config_override.as_deref())
            .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    instance::run_with_ssh(manager_conn).map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    Ok(())
}

fn handle_command(command: Command, cwd: &PathBuf, config_override: Option<&Path>) -> Result<()> {
    match command {
        Command::List => {
            let manager = SessionManager::new()?;
            let sessions = manager.list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions were found.");
                return Ok(());
            }
            let rows: Vec<tui::SessionListRow> = sessions
                .into_iter()
                .map(|session| tui::SessionListRow {
                    name: project_name(&session.directory),
                    id: session.id,
                    directory: relative_to_home(&session.directory),
                    last_active: format_last_active(session.last_active.as_deref()),
                    active: if session.active {
                        "yes".to_string()
                    } else {
                        "no".to_string()
                    },
                })
                .collect();
            tui::render_sessions_table(&rows)?;
            Ok(())
        }
        Command::Clean => {
            let instance_dir = cwd.join(session_manager::INSTANCE_DIR_NAME);
            if !instance_dir.exists() {
                println!("No .vibebox directory found at {}", instance_dir.display());
                return Ok(());
            }
            let confirmed = Confirm::new()
                .with_prompt(format!(
                    "Delete {} and all its contents?",
                    instance_dir.display()
                ))
                .default(false)
                .interact()?;
            if !confirmed {
                println!("Cancelled.");
                return Ok(());
            }
            let manager = SessionManager::new()?;
            let summary = manager.clean_project(cwd)?;
            println!(
                "Deleted {} (removed={}, session_records_removed={})",
                summary.instance_dir.display(),
                summary.removed_instance_dir,
                summary.removed_sessions
            );
            Ok(())
        }
        Command::Explain => {
            let config = config::load_config_with_path(cwd, config_override);
            let rows = build_mount_rows(cwd, &config)?;
            if rows.is_empty() {
                println!("No mounts configured.");
                return Ok(());
            }
            tui::render_mounts_table(&rows)?;
            Ok(())
        }
    }
}

fn project_name(directory: &PathBuf) -> String {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("-")
        .to_string()
}

fn relative_to_home(directory: &PathBuf) -> String {
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

fn format_last_active(value: Option<&str>) -> String {
    let Some(raw) = value else {
        return "-".to_string();
    };
    let parsed = OffsetDateTime::parse(raw, &Rfc3339);
    let Ok(timestamp) = parsed else {
        return raw.to_string();
    };
    let now = OffsetDateTime::now_utc();
    let mut seconds = (now - timestamp).whole_seconds();
    if seconds < 0 {
        seconds = 0;
    }
    let seconds = seconds as i64;
    let week_seconds = 7 * 24 * 60 * 60;
    if seconds >= week_seconds {
        let formatted =
            match time::format_description::parse("[year]-[month]-[day] [hour]:[minute]Z") {
                Ok(format) => timestamp
                    .format(&format)
                    .unwrap_or_else(|_| raw.to_string()),
                Err(_) => timestamp
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| raw.to_string()),
            };
        return formatted;
    }
    if seconds < 60 {
        return "just now".to_string();
    }
    if seconds < 60 * 60 {
        let mins = seconds / 60;
        return format!("{} min{} ago", mins, if mins == 1 { "" } else { "s" });
    }
    if seconds < 60 * 60 * 24 {
        let hours = seconds / (60 * 60);
        return format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" });
    }
    let days = seconds / (60 * 60 * 24);
    format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
}

fn build_mount_rows(cwd: &Path, config: &config::Config) -> Result<Vec<tui::MountListRow>> {
    let mut rows = Vec::new();
    rows.extend(default_mounts(cwd)?);
    for spec in &config.box_cfg.mounts {
        rows.push(parse_mount_spec(cwd, spec, false)?);
    }
    Ok(rows)
}

fn default_mounts(cwd: &Path) -> Result<Vec<tui::MountListRow>> {
    let project_name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project_guest = format!("/root/{project_name}");
    let project_host = relative_to_home(&cwd.to_path_buf());
    let mut rows = vec![tui::MountListRow {
        host: project_host,
        guest: project_guest,
        mode: "read-write".to_string(),
        default_mount: "yes".to_string(),
    }];

    let home = env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"));
    let cache_home = env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".cache"));
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

fn parse_mount_spec(cwd: &Path, spec: &str, default_mount: bool) -> Result<tui::MountListRow> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(color_eyre::eyre::eyre!("invalid mount spec: {spec}"));
    }
    let host_part = parts[0];
    let guest_part = parts[1];
    let mode = if parts.len() == 3 {
        match parts[2] {
            "read-only" => "read-only",
            "read-write" => "read-write",
            other => {
                return Err(color_eyre::eyre::eyre!(
                    "invalid mount mode '{}'; expected read-only or read-write",
                    other
                ));
            }
        }
    } else {
        "read-write"
    };
    let host_path = resolve_host_path(cwd, host_part);
    let host_display = relative_to_home(&host_path);
    let guest_display = if Path::new(guest_part).is_absolute() {
        guest_part.to_string()
    } else {
        format!("/root/{guest_part}")
    };
    Ok(tui::MountListRow {
        host: host_display,
        guest: guest_display,
        mode: mode.to_string(),
        default_mount: if default_mount { "yes" } else { "no" }.to_string(),
    })
}

fn resolve_host_path(cwd: &Path, host: &str) -> PathBuf {
    if let Some(stripped) = host.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    } else if host == "~" {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    let host_path = PathBuf::from(host);
    if host_path.is_absolute() {
        host_path
    } else {
        cwd.join(host_path)
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = std::io::stderr().is_terminal() && env::var("VIBEBOX_LOG_NO_COLOR").is_err();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(std::io::stderr)
        .try_init();
}
