use bytesize::ByteSize;
use clap::Parser;
use color_eyre::Result;
use dialoguer::Confirm;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::registry::Registry;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use vibebox::tui::{AppState, VmInfo};
use vibebox::utils::relative_to_home;
use vibebox::{
    SessionManager, commands, config, explain, instance, session_manager, tui, vm_manager,
};

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
    /// Reset the current project's .vibebox directory
    Reset,
    /// Purge the global cache directory
    PurgeCache,
    /// Explain mounts and mappings
    Explain,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let stderr_handle = init_tracing(&cwd);

    let cli = Cli::parse();
    tracing::debug!(cwd = %cwd.display(), "starting vibebox cli");
    if let Some(command) = cli.command {
        return handle_command(command, &cwd, cli.config.as_deref());
    }

    let config_override = cli.config.clone();
    let raw_args: Vec<OsString> = env::args_os().collect();
    let config = config::load_config_with_path(&cwd, config_override.as_deref())
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    let vm_info = VmInfo {
        max_memory: config.box_cfg.ram_size,
        cpu_cores: config.box_cfg.cpu_count,
        max_disk: config.box_cfg.disk_size,
        system_name: "Debian".to_string(), // TODO: read system name from the VM.
        auto_shutdown_ms: config.supervisor.auto_shutdown_ms,
    };
    if let Ok(manager) = SessionManager::new() {
        if let Err(err) = manager.update_global_sessions(&cwd) {
            tracing::warn!(error = %err, "failed to update a global session list");
        }
    } else {
        tracing::error!("failed to initialize session manager");
        std::process::exit(1);
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
    warn_disk_size_mismatch(&cwd, config.box_cfg.disk_size);
    if let Some(handle) = stderr_handle {
        let _ = handle.modify(|filter| *filter = LevelFilter::INFO);
    }

    tracing::debug!(config.supervisor.auto_shutdown_ms, "auto shutdown config");
    let manager_conn = vm_manager::ensure_manager(
        &raw_args,
        config.supervisor.auto_shutdown_ms,
        config_override.as_deref(),
    )
    .map_err(|err| {
        tracing::error!(error = %err, "failed to ensure vm manager");
        color_eyre::eyre::eyre!(err.to_string())
    })?;

    instance::run_with_ssh(manager_conn).map_err(|err| {
        tracing::error!(error = %err, "failed to ensure vm manager");
        color_eyre::eyre::eyre!(err.to_string())
    })?;

    tracing::info!("See you again — keep vibecoding (no SEVs, only vibes) 😈");

    Ok(())
}

fn handle_command(command: Command, cwd: &Path, config_override: Option<&Path>) -> Result<()> {
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
        Command::Reset => {
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
        Command::PurgeCache => {
            let cache_dir = cache_dir()?;
            if !cache_dir.exists() {
                println!("No cache directory found at {}", cache_dir.display());
                return Ok(());
            }
            let (file_count, total_bytes) = measure_dir(&cache_dir)?;
            let confirmed = Confirm::new()
                .with_prompt(format!(
                    "Delete cache directory {} and all its contents?",
                    cache_dir.display()
                ))
                .default(false)
                .interact()?;
            if !confirmed {
                println!("Cancelled.");
                return Ok(());
            }
            fs::remove_dir_all(&cache_dir)?;
            println!(
                "Purged {} file{} totaling {} from {}",
                file_count,
                if file_count == 1 { "" } else { "s" },
                ByteSize(total_bytes),
                cache_dir.display()
            );
            Ok(())
        }
        Command::Explain => {
            let config = config::load_config_with_path(cwd, config_override)
                .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
            let mounts = explain::build_mount_rows(cwd, &config)
                .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
            let networks = explain::build_network_rows(cwd)
                .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
            if mounts.is_empty() && networks.is_empty() {
                println!("No mounts or network info available.");
                return Ok(());
            }
            tui::render_explain_tables(&mounts, &networks)?;
            Ok(())
        }
    }
}

fn project_name(directory: &Path) -> String {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("-")
        .to_string()
}

fn cache_dir() -> Result<PathBuf> {
    let home = env::var("HOME").map(PathBuf::from)?;
    let cache_home = env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".cache"));
    Ok(cache_home.join(session_manager::GLOBAL_CACHE_DIR_NAME))
}

fn measure_dir(path: &Path) -> Result<(u64, u64)> {
    let mut total_bytes = 0u64;
    let mut file_count = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(path = %current.display(), error = %err, "failed to read directory");
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(path = %current.display(), error = %err, "failed to read directory entry");
                    continue;
                }
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "failed to stat path");
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                stack.push(path);
            } else {
                file_count += 1;
                total_bytes = total_bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok((file_count, total_bytes))
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

fn warn_disk_size_mismatch(cwd: &Path, configured_size: ByteSize) {
    let instance_raw = cwd
        .join(session_manager::INSTANCE_DIR_NAME)
        .join("instance.raw");
    let Ok(meta) = fs::metadata(&instance_raw) else {
        return;
    };
    let current_size = ByteSize::b(meta.len());
    if current_size == configured_size {
        return;
    }
    tracing::warn!(
        "instance disk size does not match config (current {}, config {}). \
disk_gb applies only on init. Run `vibebox reset` to recreate or set disk_gb to match; using the existing disk.",
        current_size,
        configured_size,
    );
}

type StderrHandle = reload::Handle<LevelFilter, Registry>;

fn init_tracing(cwd: &Path) -> Option<StderrHandle> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    let file_filter = filter.clone();
    let stderr_is_tty = io::stderr().is_terminal();
    let ansi = stderr_is_tty && env::var("VIBEBOX_LOG_NO_COLOR").is_err();
    let file = instance::ensure_instance_dir(cwd)
        .ok()
        .and_then(|instance_dir| {
            let log_path = instance_dir.join("cli.log");
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(log_path)
                .ok()
        });

    if stderr_is_tty {
        let (stderr_filter, handle) = reload::Layer::new(LevelFilter::INFO);
        let stderr_layer = fmt::layer()
            .with_target(false)
            .with_ansi(ansi)
            .without_time()
            .with_writer(io::stderr)
            .with_filter(stderr_filter);
        let subscriber = tracing_subscriber::registry().with(stderr_layer);
        if let Some(file) = file {
            let file_layer = fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(file)
                .with_filter(file_filter);
            let _ = subscriber.with(file_layer).try_init();
        } else {
            let _ = subscriber.try_init();
        }
        Some(handle)
    } else {
        let stderr_layer = fmt::layer()
            .with_target(false)
            .with_ansi(ansi)
            .with_writer(io::stderr)
            .with_filter(filter);
        let subscriber = tracing_subscriber::registry().with(stderr_layer);
        if let Some(file) = file {
            let file_layer = fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(file)
                .with_filter(file_filter);
            let _ = subscriber.with(file_layer).try_init();
        } else {
            let _ = subscriber.try_init();
        }
        None
    }
}
