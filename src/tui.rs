use std::{
    io::{self, Stdout},
    path::PathBuf,
    time::Duration,
};

use color_eyre::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_textarea::{Input, Key, TextArea};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ASCII_BANNER: [&str; 7] = [
    "░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░",
    "░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░",
    " ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░",
    " ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░",
    "  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░",
    "  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░",
    "   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░",
];

const STATUS_BAR_HEIGHT: u16 = 1;
const COMPLETIONS_MAX_HEIGHT: u16 = 6;
const SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

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
    pub completions: CompletionState,
    pub should_quit: bool,
    key_input_mode: KeyInputMode,
    tick: u64,
    spinner: usize,
    terminal_scroll: usize,
    input_view_width: u16,
}

impl AppState {
    pub fn new(cwd: PathBuf, vm_info: VmInfo) -> Self {
        let input = Self::default_input();

        Self {
            cwd,
            vm_info,
            history: Vec::new(),
            input,
            completions: CompletionState::default(),
            should_quit: false,
            key_input_mode: KeyInputMode::Unknown,
            tick: 0,
            spinner: 0,
            terminal_scroll: 0,
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

    pub fn activate_completions(&mut self, items: Vec<String>) {
        self.completions.set_items(items);
        self.completions.activate();
    }

    pub fn deactivate_completions(&mut self) {
        self.completions.deactivate();
    }

    pub fn toggle_completions(&mut self, items: Vec<String>) {
        if self.completions.active {
            self.deactivate_completions();
        } else {
            self.activate_completions(items);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct CompletionState {
    items: Vec<String>,
    selected: usize,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyInputMode {
    Unknown,
    Press,
    Release,
}

impl CompletionState {
    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items;
        self.selected = 0;
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
            self.items.get(self.selected).map(|s| s.as_str())
        } else {
            None
        }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn selected(&self) -> usize {
        self.selected
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutAreas {
    pub header: Rect,
    pub terminal: Rect,
    pub input: Rect,
    pub completions: Rect,
    pub status: Rect,
}

pub fn compute_layout(
    area: Rect,
    input_height: u16,
    completion_items: usize,
    completion_active: bool,
) -> LayoutAreas {
    let header_height = header_height().min(area.height);
    let mut remaining = area.height.saturating_sub(header_height);

    let input_height = input_height.max(1).min(remaining);
    remaining = remaining.saturating_sub(input_height);

    let (completion_height, status_height) = if completion_active {
        let desired = (completion_items as u16).min(COMPLETIONS_MAX_HEIGHT);
        let height = desired.min(remaining);
        (height, 0)
    } else {
        let height = STATUS_BAR_HEIGHT.min(remaining);
        (0, height)
    };

    remaining = remaining.saturating_sub(completion_height + status_height);

    let terminal_height = remaining;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Length(terminal_height),
            Constraint::Length(input_height),
            Constraint::Length(completion_height),
            Constraint::Length(status_height),
        ])
        .split(area);

    LayoutAreas {
        header: chunks[0],
        terminal: chunks[1],
        input: chunks[2],
        completions: chunks[3],
        status: chunks[4],
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
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {
                app.tick = app.tick.wrapping_add(1);
                app.spinner = (app.spinner + 1) % SPINNER_FRAMES.len();
            },
            event = events.next() => {
                if let Some(event) = event {
                    handle_event(event?, &mut app);
                }
            }
        }

        if app.should_quit {
            break;
        }
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

    if app.completions.is_active() {
        match key.code {
            KeyCode::Esc => app.deactivate_completions(),
            KeyCode::Up => app.completions.previous(),
            KeyCode::Down => app.completions.next(),
            KeyCode::Enter => {
                if let Some(selection) = app.completions.current() {
                    app.input.insert_str(selection);
                }
                app.deactivate_completions();
            }
            _ => {}
        }
        return;
    }

    if !should_handle_key_event(app, &key) {
        return;
    }

    match key.code {
        KeyCode::PageUp => {
            app.terminal_scroll = app.terminal_scroll.saturating_add(10);
        }
        KeyCode::PageDown => {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(10);
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.terminal_scroll = app.terminal_scroll.saturating_add(1);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(1);
        }
        KeyCode::Enter => {
            let message = app.take_input_text();
            if !message.trim().is_empty() {
                app.push_history(format!("> {}", message));
                app.terminal_scroll = 0;
            }
        }
        KeyCode::Tab => app.toggle_completions(default_completions()),
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
            app.terminal_scroll = app.terminal_scroll.saturating_add(3);
        }
        MouseEventKind::ScrollDown => {
            app.terminal_scroll = app.terminal_scroll.saturating_sub(3);
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

fn default_completions() -> Vec<String> {
    vec![":help".to_string(), ":new".to_string(), ":exit".to_string()]
}

fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    let area = frame.area();
    app.input_view_width = app.input_inner_width(area.width);
    let layout = compute_layout(
        area,
        app.input_height_for_width(area.width),
        app.completions.items().len(),
        app.completions.is_active(),
    );

    render_header(frame, layout.header, app);
    render_terminal(frame, layout.terminal, app);
    render_input(frame, layout.input, app);

    if app.completions.is_active() {
        render_completions(frame, layout.completions, app);
    } else {
        render_status(frame, layout.status, app);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
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

    frame.render_widget(Paragraph::new(welcome), header_chunks[0]);

    let banner_lines = ASCII_BANNER.iter().map(|line| Line::from(*line));
    frame.render_widget(
        Paragraph::new(Text::from_iter(banner_lines)),
        header_chunks[1],
    );

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

    frame.render_widget(
        Paragraph::new(info_lines).block(info_block),
        header_chunks[2],
    );
}

fn render_terminal(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let lines = app.history.iter().map(|line| Line::from(line.as_str()));
    let block = Block::default().borders(Borders::ALL).title("Terminal");
    let inner = block.inner(area);
    let inner_height = inner.height.max(1) as usize;
    let total_lines = app.history.len();
    let max_top = total_lines.saturating_sub(inner_height);
    let terminal_scroll = app.terminal_scroll.min(max_top);
    let scroll_top = max_top.saturating_sub(terminal_scroll);
    let paragraph = Paragraph::new(Text::from_iter(lines))
        .block(block)
        .wrap(Wrap { trim: true })
        .scroll((scroll_top.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    frame.render_widget(&app.input, area);
    if area.height > 0 && area.width > 0 {
        let cursor = app.input.cursor();
        let inner = match app.input.block() {
            Some(block) => block.inner(area),
            None => area,
        };
        let x = inner.x.saturating_add(cursor.1 as u16);
        let y = inner.y.saturating_add(cursor.0 as u16);
        if x < inner.x.saturating_add(inner.width) && y < inner.y.saturating_add(inner.height) {
            frame.set_cursor_position((x, y));
        }
    }
}

fn render_completions(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    if area.height == 0 {
        return;
    }

    let items: Vec<ListItem<'_>> = app
        .completions
        .items()
        .iter()
        .map(|item| ListItem::new(Line::from(item.as_str())))
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Completions"))
        .highlight_style(Style::default().fg(Color::Yellow));

    let mut state = ListState::default();
    if app.completions.is_active() && !app.completions.items().is_empty() {
        state.select(Some(app.completions.selected()));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    if area.height == 0 {
        return;
    }

    let spinner = SPINNER_FRAMES[app.spinner % SPINNER_FRAMES.len()];
    let status = Paragraph::new(Line::from(vec![
        Span::styled(":help", Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            format!("tick {} {}", app.tick, spinner),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    frame.render_widget(status, area);
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
    fn layout_without_completions_reserves_status_bar() {
        let area = Rect::new(0, 0, 80, 30);
        let layout = compute_layout(area, 3, 0, false);

        assert_eq!(layout.header.height, header_height());
        assert_eq!(layout.status.height, STATUS_BAR_HEIGHT);
        assert_eq!(layout.completions.height, 0);
        assert!(layout.terminal.height > 0);
    }

    #[test]
    fn layout_with_completions_hides_status_bar() {
        let area = Rect::new(0, 0, 80, 30);
        let layout = compute_layout(area, 4, 3, true);

        assert_eq!(layout.status.height, 0);
        assert_eq!(layout.completions.height, 3);
    }

    #[test]
    fn layout_clamps_when_space_is_tight() {
        let area = Rect::new(0, 0, 80, header_height() + 1);
        let layout = compute_layout(area, 3, 10, true);

        assert_eq!(layout.header.height, header_height());
        assert_eq!(layout.input.height, 1);
        assert_eq!(layout.terminal.height, 0);
    }

    #[test]
    fn completion_state_wraps_navigation() {
        let mut completions = CompletionState::default();
        completions.set_items(vec!["a".into(), "b".into(), "c".into()]);
        completions.activate();

        assert_eq!(completions.current(), Some("a"));

        completions.next();
        assert_eq!(completions.current(), Some("b"));

        completions.next();
        completions.next();
        assert_eq!(completions.current(), Some("a"));

        completions.previous();
        assert_eq!(completions.current(), Some("c"));
    }

    #[test]
    fn completion_state_is_inactive_when_empty() {
        let mut completions = CompletionState::default();
        completions.activate();

        assert!(!completions.is_active());
        assert_eq!(completions.current(), None);
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
