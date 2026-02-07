use std::{
    io::{self, Write},
    os::unix::io::OwnedFd,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use color_eyre::Result;
use crossterm::{
    cursor::{MoveTo, Show},
    execute, queue,
    style::{
        Attribute, Color as CrosstermColor, Print, SetAttribute, SetBackgroundColor,
        SetForegroundColor,
    },
    terminal::{Clear, ClearType},
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Widget},
};

use crate::vm;

const ASCII_BANNER: [&str; 7] = [
    "██╗   ██╗██╗██████╗ ███████╗██████╗  ██████╗ ██╗  ██╗",
    "██║   ██║██║██╔══██╗██╔════╝██╔══██╗██╔═══██╗╚██╗██╔╝",
    "██║   ██║██║██████╔╝█████╗  ██████╔╝██║   ██║ ╚███╔╝",
    "╚██╗ ██╔╝██║██╔══██╗██╔══╝  ██╔══██╗██║   ██║ ██╔██╗",
    " ╚████╔╝ ██║██████╔╝███████╗██████╔╝╚██████╔╝██╔╝ ██╗",
    "  ╚═══╝  ╚═╝╚═════╝ ╚══════╝╚═════╝  ╚═════╝ ╚═╝  ╚═╝",
    "",
];

#[derive(Debug, Clone)]
pub struct VmInfo {
    pub version: String,
    pub max_memory_mb: u64,
    pub cpu_cores: usize,
}

#[derive(Debug)]
pub struct AppState {
    pub cwd: PathBuf,
    pub vm_info: VmInfo,
    pub commands: VibeboxCommands,
}

impl AppState {
    pub fn new(cwd: PathBuf, vm_info: VmInfo, commands: VibeboxCommands) -> Self {
        Self {
            cwd,
            vm_info,
            commands,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VibeboxCommands {
    items: Vec<VibeboxCommand>,
}

impl VibeboxCommands {
    pub fn new_empty() -> Self {
        Self { items: Vec::new() }
    }

    pub fn add_command(&mut self, name: impl Into<String>, description: impl Into<String>) {
        self.items.push(VibeboxCommand {
            name: name.into(),
            description: description.into(),
        });
    }

    pub fn items(&self) -> &[VibeboxCommand] {
        &self.items
    }
}

impl Default for VibeboxCommands {
    fn default() -> Self {
        Self {
            items: vec![VibeboxCommand {
                name: ":help".to_string(),
                description: "Show Vibebox commands.".to_string(),
            }],
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct VibeboxCommand {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PageLayout {
    header: Rect,
    completions: Rect,
    total_height: u16,
}

fn compute_page_layout(app: &AppState, width: u16) -> PageLayout {
    let header_height = header_height();
    let completion_items = app.commands.items().len();
    let completion_height = if completion_items == 0 {
        0
    } else {
        (completion_items as u16).saturating_add(2)
    };
    let total_height = header_height.saturating_add(completion_height).max(1);

    let mut y = 0u16;
    let header = Rect::new(0, y, width, header_height);
    y = y.saturating_add(header_height);
    let completions = Rect::new(0, y, width, completion_height);

    PageLayout {
        header,
        completions,
        total_height,
    }
}

fn header_height() -> u16 {
    let banner_height = ASCII_BANNER.len() as u16;
    let welcome_height = 1;
    let info_height = 4;
    welcome_height + banner_height + info_height
}

pub fn render_tui_once(app: &mut AppState) -> Result<()> {
    let (width, _) = crossterm::terminal::size()?;
    if width == 0 {
        return Ok(());
    }

    let buffer = render_static_buffer(app, width);
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show)?;
    write_buffer_with_style(&buffer, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

pub fn render_commands_component(app: &mut AppState) -> Result<()> {
    let (width, _) = crossterm::terminal::size()?;
    if width == 0 {
        return Ok(());
    }

    let command_count = app.commands.items().len() as u16;
    let height = if command_count == 0 {
        0
    } else {
        command_count.saturating_add(2)
    };
    if height == 0 {
        return Ok(());
    }

    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let area = Rect::new(0, 0, width, height);
    render_completions(&mut buffer, area, app);

    let mut stdout = io::stdout();
    write_buffer_with_style(&buffer, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

pub fn passthrough_vm_io(
    app: Arc<Mutex<AppState>>,
    output_monitor: Arc<vm::OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
) -> vm::IoContext {
    vm::spawn_vm_io_with_line_handler(output_monitor, vm_output_fd, vm_input_fd, move |line| {
        if line == ":help" {
            if let Ok(mut locked) = app.lock() {
                let _ = render_commands_component(&mut locked);
            }
            return true;
        }
        false
    })
}

fn render_static_buffer(app: &mut AppState, width: u16) -> Buffer {
    let layout = compute_page_layout(app, width);
    let content_height = layout.total_height.max(1);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, content_height));
    render_header(&mut buffer, layout.header, app);
    render_completions(&mut buffer, layout.completions, app);
    buffer
}

fn write_buffer_with_style(buffer: &Buffer, out: &mut impl Write) -> io::Result<()> {
    let area = buffer.area;
    let mut current_fg: Option<CrosstermColor> = None;
    let mut current_bg: Option<CrosstermColor> = None;
    let mut current_modifier: Option<ratatui::style::Modifier> = None;

    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if cell.skip {
                continue;
            }

            let fg = map_color(cell.fg);
            let bg = map_color(cell.bg);
            let modifier = cell.modifier;
            if current_fg != Some(fg)
                || current_bg != Some(bg)
                || current_modifier != Some(modifier)
            {
                queue!(out, SetAttribute(Attribute::Reset))?;
                queue!(out, SetForegroundColor(fg), SetBackgroundColor(bg))?;
                queue_modifier(out, modifier)?;
                current_fg = Some(fg);
                current_bg = Some(bg);
                current_modifier = Some(modifier);
            }

            let symbol = cell.symbol();
            if symbol.is_empty() {
                queue!(out, Print(" "))?;
            } else {
                queue!(out, Print(symbol))?;
            }
        }
        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CrosstermColor::Reset),
            SetBackgroundColor(CrosstermColor::Reset),
            Print("\n")
        )?;
        current_fg = None;
        current_bg = None;
        current_modifier = None;
    }

    Ok(())
}

fn map_color(color: ratatui::style::Color) -> CrosstermColor {
    match color {
        ratatui::style::Color::Reset => CrosstermColor::Reset,
        ratatui::style::Color::Black => CrosstermColor::Black,
        ratatui::style::Color::Red => CrosstermColor::DarkRed,
        ratatui::style::Color::Green => CrosstermColor::DarkGreen,
        ratatui::style::Color::Yellow => CrosstermColor::DarkYellow,
        ratatui::style::Color::Blue => CrosstermColor::DarkBlue,
        ratatui::style::Color::Magenta => CrosstermColor::DarkMagenta,
        ratatui::style::Color::Cyan => CrosstermColor::DarkCyan,
        ratatui::style::Color::Gray => CrosstermColor::Grey,
        ratatui::style::Color::DarkGray => CrosstermColor::DarkGrey,
        ratatui::style::Color::LightRed => CrosstermColor::Red,
        ratatui::style::Color::LightGreen => CrosstermColor::Green,
        ratatui::style::Color::LightYellow => CrosstermColor::Yellow,
        ratatui::style::Color::LightBlue => CrosstermColor::Blue,
        ratatui::style::Color::LightMagenta => CrosstermColor::Magenta,
        ratatui::style::Color::LightCyan => CrosstermColor::Cyan,
        ratatui::style::Color::White => CrosstermColor::White,
        ratatui::style::Color::Rgb(r, g, b) => CrosstermColor::Rgb { r, g, b },
        ratatui::style::Color::Indexed(i) => CrosstermColor::AnsiValue(i),
    }
}

fn queue_modifier(out: &mut impl Write, modifier: ratatui::style::Modifier) -> io::Result<()> {
    use ratatui::style::Modifier;
    if modifier.contains(Modifier::BOLD) {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if modifier.contains(Modifier::DIM) {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if modifier.contains(Modifier::ITALIC) {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if modifier.contains(Modifier::UNDERLINED) {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if modifier.contains(Modifier::SLOW_BLINK) {
        queue!(out, SetAttribute(Attribute::SlowBlink))?;
    }
    if modifier.contains(Modifier::RAPID_BLINK) {
        queue!(out, SetAttribute(Attribute::RapidBlink))?;
    }
    if modifier.contains(Modifier::REVERSED) {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if modifier.contains(Modifier::HIDDEN) {
        queue!(out, SetAttribute(Attribute::Hidden))?;
    }
    if modifier.contains(Modifier::CROSSED_OUT) {
        queue!(out, SetAttribute(Attribute::CrossedOut))?;
    }
    Ok(())
}

fn render_header(buffer: &mut Buffer, area: Rect, app: &AppState) {
    if area.height == 0 {
        return;
    }

    let header_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(ASCII_BANNER.len() as u16),
            Constraint::Length(4),
        ])
        .split(area);

    let welcome = Line::from(vec![
        Span::raw("Welcome to Vibebox v"),
        Span::styled(&app.vm_info.version, Style::default().fg(Color::Yellow)),
    ]);

    Paragraph::new(welcome).render(header_chunks[0], buffer);

    let banner_lines = ASCII_BANNER.iter().map(|line| Line::from(*line));
    Paragraph::new(Text::from_iter(banner_lines)).render(header_chunks[1], buffer);

    let info_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_style(Style::default().fg(Color::Reset))
        .title("Session");
    let info_lines = vec![
        Line::from(vec![
            Span::raw("Directory: "),
            Span::styled(app.cwd.to_string_lossy(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("VM Version: "),
            Span::styled(&app.vm_info.version, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("CPU / Memory: "),
            Span::styled(
                format!(
                    "{} cores / {} MB",
                    app.vm_info.cpu_cores, app.vm_info.max_memory_mb
                ),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];

    Paragraph::new(info_lines)
        .block(info_block)
        .render(header_chunks[2], buffer);
}

fn render_completions(buffer: &mut Buffer, area: Rect, app: &mut AppState) {
    if area.height == 0 {
        return;
    }

    let items: Vec<ListItem<'_>> = app
        .commands
        .items()
        .iter()
        .map(|cmd| ListItem::new(Line::from(format!("{}  {}", cmd.name, cmd.description))))
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title_style(Style::default().fg(Color::Reset))
            .title("Vibebox Commands"),
    );

    list.render(area, buffer);
}
