use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use color_eyre::Result;
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

use vibebox::tui::{AppState, VmInfo};
use vibebox::{instance, tui, vm, vm_manager};

const DEFAULT_AUTO_SHUTDOWN_MS: u64 = 3000;

#[derive(Debug, Default, Deserialize)]
struct ProjectConfig {
    auto_shutdown_ms: Option<u64>,
}

fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let raw_args: Vec<OsString> = env::args_os().collect();

    if env::var("VIBEBOX_VM_MANAGER").as_deref() == Ok("1") {
        tracing::info!("starting vm manager mode");
        let args = vm::parse_cli().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
        let auto_shutdown_ms = env::var("VIBEBOX_AUTO_SHUTDOWN_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_AUTO_SHUTDOWN_MS);
        tracing::info!(auto_shutdown_ms, "vm manager config");
        if let Err(err) = vm_manager::run_manager(args, auto_shutdown_ms) {
            tracing::error!(error = %err, "vm manager exited");
            return Err(color_eyre::eyre::eyre!(err.to_string()));
        }
        return Ok(());
    }

    let args = vm::parse_cli().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    tracing::debug!("parsed cli args");
    if args.version() {
        vm::print_version();
        return Ok(());
    }
    if args.help() {
        vm::print_help();
        return Ok(());
    }

    vm::ensure_signed();

    let vm_info = VmInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        max_memory_mb: args.ram_mb(),
        cpu_cores: args.cpu_count(),
    };
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    tracing::info!(cwd = %cwd.display(), "starting vibebox cli");
    let app = Arc::new(Mutex::new(AppState::new(cwd.clone(), vm_info)));

    {
        let mut locked = app.lock().expect("app state poisoned");
        tui::render_tui_once(&mut locked)?;
    }
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout)?;
        stdout.flush()?;
    }

    let auto_shutdown_ms = load_auto_shutdown_ms(&cwd)?;
    tracing::info!(auto_shutdown_ms, "auto shutdown config");
    let manager_conn = vm_manager::ensure_manager(&raw_args, auto_shutdown_ms)
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    instance::run_with_ssh(manager_conn).map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    Ok(())
}

fn load_auto_shutdown_ms(project_root: &Path) -> Result<u64> {
    let path = project_root.join("vibebox.toml");
    let config = match fs::read_to_string(&path) {
        Ok(raw) => toml::from_str::<ProjectConfig>(&raw)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => ProjectConfig::default(),
        Err(err) => return Err(err.into()),
    };
    Ok(config.auto_shutdown_ms.unwrap_or(DEFAULT_AUTO_SHUTDOWN_MS))
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
