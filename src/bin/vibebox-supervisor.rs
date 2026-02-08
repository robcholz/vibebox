use std::{
    env,
    io::{self, IsTerminal},
};

use color_eyre::Result;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

use vibebox::{config, instance, vm, vm_manager};

fn main() -> Result<()> {
    if env::var("VIBEBOX_INTERNAL").as_deref() != Ok("1") {
        eprintln!("vibebox-supervisor is internal. Use `vibebox` instead.");
        std::process::exit(2);
    }

    init_tracing();
    color_eyre::install()?;

    tracing::info!("starting vm supervisor");
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let config = config::load_config(&cwd);
    let instance_dir = instance::ensure_instance_dir(&cwd)
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let _ = instance::touch_last_active(&instance_dir);
    let args = vm::VmArg {
        cpu_count: config.box_cfg.cpu_count,
        ram_bytes: config.box_cfg.ram_mb.saturating_mul(1024 * 1024),
        disk_bytes: config.box_cfg.disk_gb.saturating_mul(1024 * 1024 * 1024),
        no_default_mounts: false,
        mounts: config.box_cfg.mounts.clone(),
    };
    let auto_shutdown_ms = config.supervisor.auto_shutdown_ms;
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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    let stderr_is_tty = std::io::stderr().is_terminal();
    let ansi = stderr_is_tty && env::var("VIBEBOX_LOG_NO_COLOR").is_err();
    if stderr_is_tty {
        let stderr_layer = fmt::layer()
            .with_target(false)
            .with_ansi(ansi)
            .without_time()
            .with_writer(io::stderr)
            .with_filter(LevelFilter::INFO);
        let _ = tracing_subscriber::registry().with(stderr_layer).try_init();
    } else {
        let stderr_layer = fmt::layer()
            .with_target(false)
            .with_ansi(ansi)
            .with_writer(io::stderr)
            .with_filter(filter);
        let _ = tracing_subscriber::registry().with(stderr_layer).try_init();
    }
}
