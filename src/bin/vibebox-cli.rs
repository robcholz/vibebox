use std::{
    env,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
};

use color_eyre::Result;
use tracing_subscriber::EnvFilter;

use vibebox::tui::{AppState, VmInfo};
use vibebox::{SessionManager, commands, config, instance, tui, vm, vm_manager};

fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let raw_args: Vec<OsString> = env::args_os().collect();

    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    tracing::info!(cwd = %cwd.display(), "starting vibebox cli");
    let config = config::load_config(&cwd);

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
    let manager_conn = vm_manager::ensure_manager(&raw_args, auto_shutdown_ms)
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    instance::run_with_ssh(manager_conn).map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    Ok(())
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
