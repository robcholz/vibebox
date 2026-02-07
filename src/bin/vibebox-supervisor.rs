use std::{
    env,
    io::{self, IsTerminal},
};

use color_eyre::Result;
use tracing_subscriber::EnvFilter;

use vibebox::{instance, vm, vm_manager};

const DEFAULT_AUTO_SHUTDOWN_MS: u64 = 3000;

fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    tracing::info!("starting vm supervisor");
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let instance_dir = instance::ensure_instance_dir(&cwd)
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let _ = instance::touch_last_active(&instance_dir);
    let args = vm::parse_cli().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let auto_shutdown_ms = env::var("VIBEBOX_AUTO_SHUTDOWN_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_AUTO_SHUTDOWN_MS);
    tracing::info!(auto_shutdown_ms, "vm supervisor config");

    let result = vm_manager::run_manager(args, auto_shutdown_ms);
    let _ = instance::touch_last_active(&instance_dir);
    if let Err(err) = result {
        tracing::error!(error = %err, "vm supervisor exited");
        return Err(color_eyre::eyre::eyre!(err.to_string()));
    }
    tracing::info!("vm supervisor exited");

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = std::io::stderr().is_terminal() && env::var("VIBEBOX_LOG_NO_COLOR").is_err();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(io::stderr)
        .try_init();
}
