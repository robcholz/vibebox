use std::{env, ffi::OsString, path::PathBuf};

use color_eyre::Result;
use lexopt::prelude::*;

#[path = "../tui.rs"]
mod tui;

use tui::{AppState, VmInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiConfig {
    cwd: PathBuf,
    vm_version: String,
    max_memory_mb: u64,
    cpu_cores: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiCommand {
    Run(TuiConfig),
    Help,
    Version,
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Lexopt(#[from] lexopt::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let command = parse_args(env::args_os())?;
    match command {
        TuiCommand::Help => {
            print_help();
        }
        TuiCommand::Version => {
            println!("vibebox-tui {}", env!("CARGO_PKG_VERSION"));
        }
        TuiCommand::Run(config) => {
            let vm_info = VmInfo {
                version: config.vm_version,
                max_memory_mb: config.max_memory_mb,
                cpu_cores: config.cpu_cores,
            };
            let mut app = AppState::new(config.cwd, vm_info);
            app.push_history("VM output will appear here.");
            app.push_history("TODO: wire VM IO into the TUI event loop.");
            tui::run_tui(app).await?;
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        "vibebox-tui\n\nUsage:\n  vibebox-tui [options]\n\nOptions:\n  --help, -h            Show this help\n  --version             Show version\n  --cwd <path>          Working directory for the session header\n  --vm-version <ver>    VM version string for the header\n  --max-memory <mb>     Max memory in MB (default 2048)\n  --cpu-cores <count>   CPU core count (default 2)\n"
    );
}

fn parse_args<I>(args: I) -> Result<TuiCommand, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    fn os_to_string(value: OsString, flag: &str) -> Result<String, CliError> {
        value
            .into_string()
            .map_err(|_| CliError::Message(format!("{flag} expects valid UTF-8")))
    }

    let mut parser = lexopt::Parser::from_iter(args);
    let mut cwd: Option<PathBuf> = None;
    let mut vm_version = env!("CARGO_PKG_VERSION").to_string();
    let mut max_memory_mb: u64 = 2048;
    let mut cpu_cores: usize = 2;

    while let Some(arg) = parser.next()? {
        match arg {
            Long("help") | Short('h') => return Ok(TuiCommand::Help),
            Long("version") => return Ok(TuiCommand::Version),
            Long("cwd") => {
                let value = os_to_string(parser.value()?, "--cwd")?;
                cwd = Some(PathBuf::from(value));
            }
            Long("vm-version") => {
                vm_version = os_to_string(parser.value()?, "--vm-version")?;
            }
            Long("max-memory") => {
                let value: u64 = os_to_string(parser.value()?, "--max-memory")?
                    .parse()
                    .map_err(|_| {
                        CliError::Message("--max-memory expects an integer".to_string())
                    })?;
                if value == 0 {
                    return Err(CliError::Message("--max-memory must be >= 1".to_string()));
                }
                max_memory_mb = value;
            }
            Long("cpu-cores") => {
                let value: usize = os_to_string(parser.value()?, "--cpu-cores")?
                    .parse()
                    .map_err(|_| CliError::Message("--cpu-cores expects an integer".to_string()))?;
                if value == 0 {
                    return Err(CliError::Message("--cpu-cores must be >= 1".to_string()));
                }
                cpu_cores = value;
            }
            Value(value) => {
                return Err(CliError::Message(format!(
                    "unexpected argument: {}",
                    value.to_string_lossy()
                )));
            }
            _ => return Err(CliError::Message(arg.unexpected().to_string())),
        }
    }

    let cwd = match cwd {
        Some(dir) => dir,
        None => env::current_dir()?,
    };

    Ok(TuiCommand::Run(TuiConfig {
        cwd,
        vm_version,
        max_memory_mb,
        cpu_cores,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_from(args: &[&str]) -> Result<TuiCommand, CliError> {
        let mut argv = vec![OsString::from("vibebox-tui")];
        argv.extend(args.iter().map(OsString::from));
        parse_args(argv)
    }

    #[test]
    fn parse_help_short_circuit() {
        let command = parse_from(&["--help"]).unwrap();
        assert!(matches!(command, TuiCommand::Help));
    }

    #[test]
    fn parse_version_short_circuit() {
        let command = parse_from(&["--version"]).unwrap();
        assert!(matches!(command, TuiCommand::Version));
    }

    #[test]
    fn parse_defaults() {
        let command = parse_from(&[]).unwrap();
        let TuiCommand::Run(config) = command else {
            panic!("expected run command");
        };

        assert_eq!(config.vm_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(config.max_memory_mb, 2048);
        assert_eq!(config.cpu_cores, 2);
    }

    #[test]
    fn parse_overrides() {
        let command = parse_from(&[
            "--cwd",
            "/tmp",
            "--vm-version",
            "13.1",
            "--max-memory",
            "4096",
            "--cpu-cores",
            "4",
        ])
        .unwrap();

        let TuiCommand::Run(config) = command else {
            panic!("expected run command");
        };

        assert_eq!(config.cwd, PathBuf::from("/tmp"));
        assert_eq!(config.vm_version, "13.1");
        assert_eq!(config.max_memory_mb, 4096);
        assert_eq!(config.cpu_cores, 4);
    }

    #[test]
    fn parse_rejects_zero_cpu() {
        let err = parse_from(&["--cpu-cores", "0"]).unwrap_err();
        assert!(err.to_string().contains("cpu-cores"));
    }

    #[test]
    fn parse_rejects_zero_memory() {
        let err = parse_from(&["--max-memory", "0"]).unwrap_err();
        assert!(err.to_string().contains("max-memory"));
    }

    #[test]
    fn parse_rejects_unknown_argument() {
        let err = parse_from(&["--unknown"]).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
