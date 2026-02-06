use std::{
    io::{self, Stdout, Write},
    path::PathBuf,
};

use color_eyre::Result;
use crossterm::{
    cursor::{MoveTo, Show},
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute, queue,
    style::{
        Attribute, Color as CrosstermColor, Print, SetAttribute, SetBackgroundColor,
        SetForegroundColor,
    },
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, StatefulWidget, Widget},
};
use tui_textarea::{Input, Key, TextArea};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    pub history: Vec<String>,
    pub input: TextArea<'static>,
    pub commands: VibeboxCommands,
    pub should_quit: bool,
    key_input_mode: KeyInputMode,
    page_scroll: usize,
    input_view_width: u16,
}

impl AppState {
    pub fn new(cwd: PathBuf, vm_info: VmInfo) -> Self {
        let input = Self::default_input();
        let mut commands = VibeboxCommands::default();
        commands.add_command(":new", "Create a new session.");
        commands.add_command(":exit", "Exit Vibebox.");

        Self {
            cwd,
            vm_info,
            history: Vec::new(),
            input,
            commands,
            should_quit: false,
            key_input_mode: KeyInputMode::Unknown,
            page_scroll: 0,
            input_view_width: 0,
        }
    }

    fn default_input() -> TextArea<'static> {
        let mut input = TextArea::default();
        input.set_cursor_style(Style::default().fg(Color::Yellow));
        input.set_block(Block::default().borders(Borders::ALL).title("Input"));
        input
    }

    fn reset_input(&mut self) {
        self.input = Self::default_input();
    }

    fn take_input_text(&mut self) -> String {
        let text = self.input.lines().join("");
        self.reset_input();
        text
    }

    pub fn input_height_for_width(&self, available_width: u16) -> u16 {
        let inner_width = self.input_inner_width(available_width).max(1) as usize;
        let mut visual_lines = 0usize;
        for line in self.input.lines() {
            let line_width = UnicodeWidthStr::width(line.as_str());
            let wrapped = if line_width == 0 {
                1
            } else {
                (line_width + inner_width - 1) / inner_width
            };
            visual_lines += wrapped;
        }

        let mut height = (visual_lines.max(1) as u16).max(1);
        if self.input.block().is_some() {
            height = height.saturating_add(2);
        }
        height.max(1)
    }

    fn input_inner_width(&self, available_width: u16) -> u16 {
        if self.input.block().is_some() {
            available_width.saturating_sub(2)
        } else {
            available_width
        }
    }

    fn insert_char_with_wrap(&mut self, c: char) {
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0);
        let width = self.input_view_width.max(1) as usize;
        let mut should_wrap = false;
        if char_width > 0 && width > 0 {
            let (row, col) = self.input.cursor();
            if let Some(line) = self.input.lines().get(row) {
                if col == line.chars().count() {
                    let line_width = UnicodeWidthStr::width(line.as_str());
                    should_wrap = line_width + char_width > width;
                }
            }
        }

        if should_wrap {
            if c == ' ' {
                self.input.insert_char(c);
                self.input.insert_newline();
            } else {
                self.input.insert_newline();
                self.input.insert_char(c);
            }
        } else {
            self.input.insert_char(c);
        }
    }

    fn insert_str_with_wrap(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.insert_char_with_wrap(' '),
                '\r' => {}
                _ => self.insert_char_with_wrap(c),
            }
        }
    }

    pub fn push_history(&mut self, line: impl Into<String>) {
        self.history.push(line.into());
        if self.history.len() > 2000 {
            let excess = self.history.len() - 2000;
            self.history.drain(0..excess);
        }
    }

    pub fn activate_commands(&mut self) {
        self.commands.activate();
    }

    pub fn deactivate_commands(&mut self) {
        self.commands.deactivate();
    }

    pub fn toggle_commands(&mut self) {
        if self.commands.active {
            self.deactivate_commands();
        } else {
            self.activate_commands();
        }
    }
}

#[derive(Debug, Clone)]
pub struct VibeboxCommands {
    items: Vec<VibeboxCommand>,
    selected: usize,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyInputMode {
    Unknown,
    Press,
    Release,
}

impl VibeboxCommands {
    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items.into_iter().map(VibeboxCommand::new).collect();
        self.selected = 0;
    }

    pub fn add_command(&mut self, name: impl Into<String>, description: impl Into<String>) {
        self.items.push(VibeboxCommand {
            name: name.into(),
            description: description.into(),
        });
    }

    pub fn activate(&mut self) {
        self.active = !self.items.is_empty();
        self.selected = 0;
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    pub fn next(&mut self) {
        if !self.active || self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    pub fn previous(&mut self) {
        if !self.active || self.items.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.items.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn current(&self) -> Option<&str> {
        if self.active {
            self.items.get(self.selected).map(|cmd| cmd.name.as_str())
        } else {
            None
        }
    }

    pub fn items(&self) -> &[VibeboxCommand] {
        &self.items
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn selected(&self) -> usize {
        self.selected
    }
}

impl Default for VibeboxCommands {
    fn default() -> Self {
        Self {
            items: vec![VibeboxCommand {
                name: ":help".to_string(),
                description: "Show Vibebox commands.".to_string(),
            }],
            selected: 0,
            active: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct VibeboxCommand {
    pub name: String,
    pub description: String,
}

impl VibeboxCommand {
    pub fn new(name: String) -> Self {
        let description = command_description(&name).to_string();
        Self { name, description }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutAreas {
    pub header: Rect,
    pub terminal: Rect,
    pub input: Rect,
    pub completions: Rect,
    pub status: Rect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PageLayout {
    header: Rect,
    terminal: Rect,
    input: Rect,
    completions: Rect,
    status: Rect,
    total_height: u16,
}

#[allow(dead_code)]
pub fn compute_layout(area: Rect, input_height: u16, completion_items: usize) -> LayoutAreas {
    let header_height = header_height().min(area.height);
    let mut remaining = area.height.saturating_sub(header_height);

    let input_height = input_height.max(1).min(remaining);
    remaining = remaining.saturating_sub(input_height);

    let completion_height = if completion_items == 0 {
        0
    } else {
        let desired = (completion_items as u16).saturating_add(2);
        desired
    };
    let completion_height = completion_height.min(remaining);

    let terminal_height = 0;

    let mut y = 0u16;
    let header = Rect::new(area.x, area.y + y, area.width, header_height);
    y = y.saturating_add(header_height);
    let terminal = Rect::new(area.x, area.y + y, area.width, terminal_height);
    let input = Rect::new(area.x, area.y + y, area.width, input_height);
    y = y.saturating_add(input_height);
    let completions = Rect::new(area.x, area.y + y, area.width, completion_height);
    let status = Rect::new(area.x, area.y + y, area.width, 0);

    LayoutAreas {
        header,
        terminal,
        input,
        completions,
        status,
    }
}

fn compute_page_layout(app: &AppState, width: u16) -> PageLayout {
    let header_height = header_height();
    let input_height = 0;
    let completion_items = app.commands.items().len();
    let completion_height = if completion_items == 0 {
        0
    } else {
        (completion_items as u16).saturating_add(2)
    };
    let total_height = header_height
        .saturating_add(input_height)
        .saturating_add(completion_height)
        .max(1);

    let mut y = 0u16;
    let header = Rect::new(0, y, width, header_height);
    y = y.saturating_add(header_height);
    let terminal = Rect::new(0, y, width, 0);
    let input = Rect::new(0, y, width, input_height);
    y = y.saturating_add(input_height);
    let completions = Rect::new(0, y, width, completion_height);
    let status = Rect::new(0, y, width, 0);

    PageLayout {
        header,
        terminal,
        input,
        completions,
        status,
        total_height,
    }
}

fn header_height() -> u16 {
    let banner_height = ASCII_BANNER.len() as u16;
    let welcome_height = 1;
    let info_height = 4;
    welcome_height + banner_height + info_height
}

pub async fn run_tui(mut app: AppState) -> Result<()> {
    let mut terminal = TerminalGuard::init()?;
    let mut events = EventStream::new();

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        if let Some(event) = events.next().await {
            handle_event(event?, &mut app);
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
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

fn handle_event(event: CrosstermEvent, app: &mut AppState) {
    match event {
        CrosstermEvent::Key(key) => handle_key_event(key, app),
        CrosstermEvent::Resize(_, _) => {}
        CrosstermEvent::Mouse(event) => handle_mouse_event(event, app),
        CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => {}
        CrosstermEvent::Paste(text) => {
            app.insert_str_with_wrap(&text);
        }
    }
}

fn handle_key_event(key: KeyEvent, app: &mut AppState) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    if app.commands.is_active() {
        match key.code {
            KeyCode::Esc => app.deactivate_commands(),
            KeyCode::Up => app.commands.previous(),
            KeyCode::Down => app.commands.next(),
            KeyCode::Enter => {
                if let Some(selection) = app.commands.current() {
                    app.input.insert_str(selection);
                }
                app.deactivate_commands();
            }
            _ => {}
        }
        return;
    }

    if !should_handle_key_event(app, &key) {
        return;
    }

    match key.code {
        KeyCode::PageUp => {}
        KeyCode::PageDown => {}
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {}
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {}
        KeyCode::Enter => {
            let message = app.take_input_text();
            if !message.trim().is_empty() {
                app.push_history(format!("> {}", message));
                app.page_scroll = usize::MAX;
            }
        }
        KeyCode::Tab => app.toggle_commands(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.insert_char_with_wrap(c);
        }
        _ => {
            app.input.input(input_from_key_event(key));
        }
    }
}

fn handle_mouse_event(event: crossterm::event::MouseEvent, app: &mut AppState) {
    use crossterm::event::MouseEventKind;
    match event.kind {
        MouseEventKind::ScrollUp => {
            app.page_scroll = app.page_scroll.saturating_sub(3);
        }
        MouseEventKind::ScrollDown => {
            app.page_scroll = app.page_scroll.saturating_add(3);
        }
        _ => {}
    }
}

fn should_handle_key_event(app: &mut AppState, key: &KeyEvent) -> bool {
    match key.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => {
            app.key_input_mode = KeyInputMode::Press;
            return true;
        }
        KeyEventKind::Release => {}
    }

    if app.key_input_mode == KeyInputMode::Press {
        return false;
    }

    if key.code == KeyCode::Null {
        return false;
    }

    if app.key_input_mode == KeyInputMode::Unknown {
        app.key_input_mode = KeyInputMode::Release;
    }

    true
}

fn input_from_key_event(key: KeyEvent) -> Input {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let key = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        _ => Key::Null,
    };

    Input {
        key,
        ctrl,
        alt,
        shift,
    }
}

fn command_description(command: &str) -> &'static str {
    match command {
        ":help" => "Show Vibebox commands.",
        ":new" => "Create a new session.",
        ":exit" => "Exit Vibebox.",
        _ => "",
    }
}

fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    let viewport = frame.area();
    if viewport.width == 0 || viewport.height == 0 {
        return;
    }

    app.input_view_width = app.input_inner_width(viewport.width);
    let layout = compute_page_layout(app, viewport.width);
    let content_height = layout.total_height.max(1);

    let mut buffer = Buffer::empty(Rect::new(0, 0, viewport.width, content_height));
    render_header(&mut buffer, layout.header, app);
    render_completions(&mut buffer, layout.completions, app);

    let max_scroll = content_height.saturating_sub(viewport.height);
    app.page_scroll = app.page_scroll.min(max_scroll as usize);
    let scroll = app.page_scroll as u16;

    let view = frame.buffer_mut();
    for y in 0..viewport.height {
        let src_y = scroll.saturating_add(y);
        if src_y >= content_height {
            break;
        }
        for x in 0..viewport.width {
            view[(x, y)] = buffer[(x, src_y)].clone();
        }
    }
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

    let info_block = Block::default().borders(Borders::ALL).title("Session");
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

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Vibebox Commands"),
        )
        .highlight_style(Style::default().fg(Color::Yellow));

    let mut state = ListState::default();
    if app.commands.is_active() && !app.commands.items().is_empty() {
        state.select(Some(app.commands.selected()));
    }

    StatefulWidget::render(list, area, buffer, &mut state);
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn init() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    fn draw(&mut self, f: impl FnOnce(&mut Frame<'_>)) -> Result<()> {
        self.terminal.draw(f)?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_without_completions_hides_status_bar() {
        let area = Rect::new(0, 0, 80, 30);
        let layout = compute_layout(area, 3, 0);

        assert_eq!(layout.header.height, header_height());
        assert_eq!(layout.status.height, 0);
        assert_eq!(layout.completions.height, 0);
        assert_eq!(layout.terminal.height, 0);
    }

    #[test]
    fn layout_with_completions_hides_status_bar() {
        let area = Rect::new(0, 0, 80, 30);
        let layout = compute_layout(area, 4, 3);

        assert_eq!(layout.status.height, 0);
        assert_eq!(layout.completions.height, 5);
    }

    #[test]
    fn layout_clamps_when_space_is_tight() {
        let area = Rect::new(0, 0, 80, header_height() + 1);
        let layout = compute_layout(area, 3, 10);

        assert_eq!(layout.header.height, header_height());
        assert_eq!(layout.input.height, 1);
        assert_eq!(layout.terminal.height, 0);
    }

    #[test]
    fn commands_wrap_navigation() {
        let mut commands = VibeboxCommands::default();
        commands.set_items(vec![":new".into(), ":exit".into(), ":help".into()]);
        commands.activate();

        assert_eq!(commands.current(), Some(":new"));

        commands.next();
        assert_eq!(commands.current(), Some(":exit"));

        commands.next();
        commands.next();
        assert_eq!(commands.current(), Some(":new"));

        commands.previous();
        assert_eq!(commands.current(), Some(":help"));
    }

    #[test]
    fn commands_inactive_when_empty() {
        let mut commands = VibeboxCommands::default();
        commands.activate();

        assert!(!commands.is_active());
        assert_eq!(commands.current(), None);
    }

    #[test]
    fn input_from_key_event_maps_char_and_modifiers() {
        let key = KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let input = input_from_key_event(key);

        assert_eq!(input.key, Key::Char('x'));
        assert!(input.ctrl);
        assert!(input.alt);
        assert!(!input.shift);
    }

    #[test]
    fn input_from_key_event_maps_special_keys() {
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        let input = input_from_key_event(key);

        assert_eq!(input.key, Key::Backspace);
        assert!(!input.ctrl);
        assert!(!input.alt);
        assert!(!input.shift);
    }
}
