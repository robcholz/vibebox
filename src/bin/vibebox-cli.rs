use std::{
    env,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use color_eyre::Result;

use vibebox::tui::{AppState, VmInfo};
use vibebox::{tui, vm};

fn main() -> Result<()> {
    color_eyre::install()?;

    let args = vm::parse_cli().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    if args.version() {
        vm::print_version();
        return Ok(());
    }
    if args.help() {
        vm::print_help();
        return Ok(());
    }

    let vm_info = VmInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        max_memory_mb: args.ram_mb(),
        cpu_cores: args.cpu_count(),
    };
    let cwd = env::current_dir().map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
    let app = Arc::new(Mutex::new(AppState::new(cwd, vm_info)));

    {
        let mut locked = app.lock().expect("app state poisoned");
        tui::render_tui_once(&mut locked)?;
    }
    {
        let mut stdout = io::stdout().lock();
        writeln!(stdout)?;
        stdout.flush()?;
    }

    vm::run_with_args(args, |output_monitor, vm_output_fd, vm_input_fd| {
        tui::passthrough_vm_io(app.clone(), output_monitor, vm_output_fd, vm_input_fd)
    })
    .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;

    Ok(())
}
