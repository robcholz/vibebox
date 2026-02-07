use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::tui::{AppState, VibeboxCommands};
use crate::vm::IoControl;

#[derive(Clone, Copy)]
enum CommandKind {
    Help,
    Exit,
}

struct CommandSpec {
    name: &'static str,
    description: &'static str,
    kind: CommandKind,
    shell_alias: Option<&'static str>,
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        name: ":help",
        description: "Show Vibebox commands.",
        kind: CommandKind::Help,
        shell_alias: Some("vibebox_help"),
    },
    CommandSpec {
        name: ":exit",
        description: "Exit Vibebox.",
        kind: CommandKind::Exit,
        shell_alias: Some("exit"),
    },
];

pub struct CommandHandlers {
    handlers: HashMap<String, Box<dyn Fn() + Send + Sync>>,
}

impl CommandHandlers {
    fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    fn register<F>(&mut self, name: &str, handler: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.handlers.insert(name.to_string(), Box::new(handler));
    }

    pub fn handle(&self, line: &str) -> bool {
        if let Some(handler) = self.handlers.get(line) {
            handler();
            true
        } else {
            false
        }
    }
}

pub fn build_commands() -> VibeboxCommands {
    let mut commands = VibeboxCommands::new_empty();
    for spec in COMMAND_SPECS {
        commands.add_command(spec.name, spec.description);
    }
    commands
}

pub fn render_shell_script() -> String {
    let mut lines = Vec::new();
    lines.push("vibebox_help() {".to_string());
    lines.push("  cat <<'VIBEBOX_HELP'".to_string());
    lines.push("Vibebox Commands".to_string());
    for spec in COMMAND_SPECS {
        lines.push(format!("{}  {}", spec.name, spec.description));
    }
    lines.push("VIBEBOX_HELP".to_string());
    lines.push("}".to_string());
    for spec in COMMAND_SPECS {
        if let Some(alias) = spec.shell_alias {
            lines.push(format!("alias {}='{}'", spec.name, alias));
        }
    }
    lines.join("\n")
}

pub fn build_handlers(app: Arc<Mutex<AppState>>, io_control: Arc<IoControl>) -> CommandHandlers {
    let mut handlers = CommandHandlers::new();
    for spec in COMMAND_SPECS {
        match spec.kind {
            CommandKind::Help => {
                let app = app.clone();
                handlers.register(spec.name, move || {
                    if let Ok(mut locked) = app.lock() {
                        let _ = crate::tui::render_commands_component(&mut locked);
                    }
                });
            }
            CommandKind::Exit => {
                let io_control = io_control.clone();
                handlers.register(spec.name, move || {
                    io_control.request_terminal_restore();
                    std::process::exit(0);
                });
            }
        }
    }
    handlers
}
