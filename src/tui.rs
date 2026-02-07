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
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Widget},
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
const INFO_LINE_COUNT: u16 = 5;

#[derive(Debug, Clone)]
pub struct VmInfo {
    pub max_memory_mb: u64,
    pub cpu_cores: usize,
    pub system_name: String,
    pub auto_shutdown_ms: u64,
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

#[derive(Debug, Clone)]
pub struct SessionListRow {
    pub name: String,
    pub directory: String,
    pub last_active: String,
    pub active: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct MountListRow {
    pub host: String,
    pub guest: String,
    pub mode: String,
    pub default_mount: String,
}

#[derive(Debug, Clone)]
pub struct NetworkListRow {
    pub network_type: String,
    pub vm_ip: String,
    pub host_to_vm: String,
    pub vm_to_host: String,
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
    let info_height = info_block_height();
    welcome_height + banner_height + info_height
}

fn info_block_height() -> u16 {
    INFO_LINE_COUNT.saturating_add(1)
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

pub fn render_sessions_table(rows: &[SessionListRow]) -> Result<()> {
    let (width, _) = crossterm::terminal::size()?;
    if width == 0 {
        return Ok(());
    }

    let height = (rows.len() as u16).saturating_add(3);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let area = Rect::new(0, 0, width, height);

    let header = Row::new(vec![
        Cell::from("Name"),
        Cell::from("Last Active"),
        Cell::from("Active"),
        Cell::from("ID"),
        Cell::from("Directory"),
    ])
    .style(Style::default().fg(Color::Cyan));

    let table_rows = rows.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.name.clone()),
            Cell::from(row.last_active.clone()),
            Cell::from(row.active.clone()),
            Cell::from(row.id.clone()),
            Cell::from(row.directory.clone()),
        ])
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(16),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(36),
            Constraint::Min(24),
        ],
    )
    .header(header)
    .block(Block::default().title("Sessions").borders(Borders::ALL))
    .column_spacing(2);

    table.render(area, &mut buffer);

    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show)?;
    write_buffer_with_style(&buffer, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

pub fn render_mounts_table(rows: &[MountListRow]) -> Result<()> {
    let (width, _) = crossterm::terminal::size()?;
    if width == 0 {
        return Ok(());
    }

    let height = (rows.len() as u16).saturating_add(3);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let area = Rect::new(0, 0, width, height);

    render_mounts_table_into(rows, area, &mut buffer);

    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show)?;
    write_buffer_with_style(&buffer, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

pub fn render_explain_tables(mounts: &[MountListRow], networks: &[NetworkListRow]) -> Result<()> {
    let (width, _) = crossterm::terminal::size()?;
    if width == 0 {
        return Ok(());
    }

    let mounts_height = if mounts.is_empty() {
        0
    } else {
        (mounts.len() as u16).saturating_add(3)
    };
    let networks_height = if networks.is_empty() {
        0
    } else {
        (networks.len() as u16).saturating_add(3)
    };
    let gap = if mounts_height > 0 && networks_height > 0 {
        1
    } else {
        0
    };
    let total_height = mounts_height
        .saturating_add(gap)
        .saturating_add(networks_height);
    if total_height == 0 {
        return Ok(());
    }

    let mut buffer = Buffer::empty(Rect::new(0, 0, width, total_height));
    let mut y = 0u16;

    if mounts_height > 0 {
        let area = Rect::new(0, y, width, mounts_height);
        render_mounts_table_into(mounts, area, &mut buffer);
        y = y.saturating_add(mounts_height).saturating_add(gap);
    }

    if networks_height > 0 {
        let area = Rect::new(0, y, width, networks_height);
        render_networks_table_into(networks, area, &mut buffer);
    }

    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show)?;
    write_buffer_with_style(&buffer, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

fn render_mounts_table_into(rows: &[MountListRow], area: Rect, buffer: &mut Buffer) {
    let header = Row::new(vec![
        Cell::from("Host"),
        Cell::from("Guest"),
        Cell::from("Mode"),
        Cell::from(""),
        Cell::from("Default"),
    ])
    .style(Style::default().fg(Color::Cyan));

    let table_rows = rows.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.host.clone()),
            Cell::from(row.guest.clone()),
            Cell::from(row.mode.clone()),
            Cell::from(""),
            Cell::from(row.default_mount.clone()),
        ])
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Min(24),
            Constraint::Min(24),
            Constraint::Length(10),
            Constraint::Length(1),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().title("Mounts").borders(Borders::ALL))
    .column_spacing(1);

    table.render(area, buffer);
}

fn render_networks_table_into(rows: &[NetworkListRow], area: Rect, buffer: &mut Buffer) {
    let header = Row::new(vec![
        Cell::from("Type"),
        Cell::from("VM IP"),
        Cell::from("Host \u{2192} VM"),
        Cell::from("VM \u{2192} Host"),
    ])
    .style(Style::default().fg(Color::Cyan));

    let table_rows = rows.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.network_type.clone()),
            Cell::from(row.vm_ip.clone()),
            Cell::from(row.host_to_vm.clone()),
            Cell::from(row.vm_to_host.clone()),
        ])
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(8),
            Constraint::Length(16),
            Constraint::Min(24),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(Block::default().title("Network").borders(Borders::ALL))
    .column_spacing(1);

    table.render(area, buffer);
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
            Constraint::Length(info_block_height()),
        ])
        .split(area);

    let version = env!("CARGO_PKG_VERSION");

    let welcome = Line::from(vec![
        Span::raw("Welcome to Vibebox v"),
        Span::styled(version, Style::default().fg(Color::Yellow)),
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
            Span::raw("System: "),
            Span::styled(&app.vm_info.system_name, Style::default().fg(Color::Green)),
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
        Line::from(vec![
            Span::raw("Auto Shutdown: "),
            Span::styled(
                format!("{} ms", app.vm_info.auto_shutdown_ms),
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
