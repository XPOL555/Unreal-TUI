use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use serde::Deserialize;

/* ------------------------- Config structures ------------------------- */

#[derive(Debug, Deserialize)]
struct Config {
    projects: Vec<Project>,
}
#[derive(Debug, Clone, Deserialize)]
struct Project {
    key: String,               // e.g. "game" or "pcg"
    #[serde(default)]
    name: String,              // pretty name
    uproject: PathBuf,         // absolute or relative path to .uproject
}

/* --------------------------- App structures -------------------------- */

#[derive(PartialEq)]
enum Mode {
    Select,         // choose a project
    View,           // show tail of log
}

#[derive(Clone)]
struct LogLine {
    // original full line as read
    text: String,
    color: Color,
    // parsed pieces for richer rendering
    ts: Option<String>,           // content of first [ ... ]
    category: Option<String>,     // e.g., LogRenderer
    message: String,              // remainder after category and colon
}

enum Cmd {
    Clear,          // jump tail offset to EOF
}

enum AppEvent {
    Line(LogLine),
    Error(String),
    Tick,
}

/* ------------------------------ Main -------------------------------- */

fn main() -> Result<()> {
    // Load config before touching the terminal.
    let cfg = load_config().context("Cannot load projects.json")?;
    if cfg.projects.is_empty() {
        return Err(anyhow!("projects.json has no projects"));
    }

    // Terminal init
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::prelude::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(cfg);

    // UI/Event loop
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| app.draw(f))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        // Input
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) => {
                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                    if app.on_key(k.kind, k.code, ctrl)? == Action::Quit { break; }
                }
                Event::Mouse(m) => { app.on_mouse(m); }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Ticks + log lines
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
            while let Ok(ev) = app.rx.try_recv() {
                match ev {
                    AppEvent::Line(l) => app.push_line(l),
                    AppEvent::Error(e) => app.last_error = Some(e),
                    AppEvent::Tick => {},
                }
            }
        }
    }

    // Teardown
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/* ------------------------------- App -------------------------------- */

struct App {
    mode: Mode,
    cfg: Config,
    // selection
    selected: usize,
    // view
    current: Option<Project>,
    lines: Vec<LogLine>,
    scroll_from_bottom: usize, // 0 = bottom, grows when user scrolls up
    last_error: Option<String>,
    // rendering state / options
    show_timestamp: bool,                  // toggleable, default off
    active_category_filter: Option<String>,
    last_body_area: Rect,                  // for mouse hit testing
    // tail thread channels
    rx: mpsc::Receiver<AppEvent>,
    tx_cmd: mpsc::Sender<Cmd>,
}

#[derive(PartialEq)]
enum Action { Continue, Quit }

impl App {
    fn new(cfg: Config) -> Self {
        let (tx_ev, rx) = mpsc::channel::<AppEvent>();
        let (tx_cmd, rx_cmd) = mpsc::channel::<Cmd>();
        // idle tail thread doing nothing until a project is started
        spawn_idle_tail(tx_ev.clone(), rx_cmd);
        Self {
            mode: Mode::Select,
            cfg,
            selected: 0,
            current: None,
            lines: Vec::new(),
            scroll_from_bottom: 0,
            last_error: None,
            show_timestamp: false,
            active_category_filter: None,
            last_body_area: Rect::new(0, 0, 0, 0),
            rx,
            tx_cmd,
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let size = f.size();

        match self.mode {
            Mode::Select => {
                let items: Vec<ListItem> = self.cfg.projects.iter().map(|p| {
                    let title = if p.name.is_empty() { p.key.clone() } else { p.name.clone() };
                    let path = p.uproject.display().to_string();
                    ListItem::new(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(title, Style::default().fg(Color::Cyan)),
                        Span::raw("\n  "),
                        Span::styled(path, Style::default().fg(Color::DarkGray)),
                    ]))
                }).collect();

                let list = List::new(items)
                    .block(Block::default().title("Select project (Enter) — Quit: Q").borders(Borders::ALL))
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

                f.render_stateful_widget(list, size, &mut ratatui::widgets::ListState::default().with_selected(Some(self.selected)));
            }
            Mode::View => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)].as_ref())
                    .split(size);

                // Header: left project title/help, right filter info
                let left_title = if let Some(p) = &self.current {
                    format!(" {}  —  Clear: C | Scroll: ↑/↓ PgUp/PgDn Home/End | Toggle TS: T | Clear Filter: F | Switch Project: S | Quit: Q ", p.name_or_key())
                } else {
                    " <no project> ".to_string()
                };
                let right_title = if let Some(cat) = &self.active_category_filter {
                    format!("Filter: {} (clear: F)", cat)
                } else { String::new() };
                let hchunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(70), Constraint::Percentage(30)].as_ref())
                    .split(chunks[0]);
                let header_left = Paragraph::new(left_title).style(Style::default().fg(Color::Cyan));
                let header_right = Paragraph::new(right_title).style(Style::default().fg(Color::Yellow)).alignment(Alignment::Right);
                f.render_widget(header_left, hchunks[0]);
                f.render_widget(header_right, hchunks[1]);

                // Prepare filtered lines
                let filtered: Vec<&LogLine> = if let Some(cat) = &self.active_category_filter {
                    self.lines.iter().filter(|l| l.category.as_deref() == Some(cat.as_str())).collect()
                } else {
                    self.lines.iter().collect()
                };

                // Log body – compute visible slice based on scroll_from_bottom
                let h = chunks[1].height as usize;
                let total = filtered.len();
                let end = total.saturating_sub(self.scroll_from_bottom);
                let start = end.saturating_sub(h);
                let slice = &filtered[start..end];

                // remember body area for mouse clicks
                self.last_body_area = chunks[1];

                let mut lines_vec: Vec<Line> = Vec::with_capacity(slice.len());
                for l in slice.iter() {
                    let mut spans: Vec<Span> = Vec::new();
                    if self.show_timestamp {
                        if let Some(ts) = &l.ts {
                            spans.push(Span::styled(format!("[{}] ", ts), Style::default().fg(Color::DarkGray)));
                        }
                    }
                    if let Some(cat) = &l.category {
                        spans.push(Span::styled(format!("{}:", cat), Style::default().add_modifier(Modifier::UNDERLINED).fg(Color::Cyan)));
                        spans.push(Span::raw(" "));
                    }
                    // message (or original text if no parsed parts)
                    let msg = if l.category.is_some() || l.ts.is_some() { l.message.as_str() } else { l.text.as_str() };
                    spans.push(Span::styled(msg, Style::default().fg(l.color)));
                    lines_vec.push(Line::from(spans));
                }

                let body = Paragraph::new(lines_vec)
                    .block(Block::default().borders(Borders::ALL).title("Logs"))
                    .scroll((0, 0));
                f.render_widget(body, chunks[1]);

                // Footer status – not red, italic preferred
                let footer = Paragraph::new(
                    self.last_error.clone().unwrap_or_default()
                ).style(Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC));
                f.render_widget(footer, chunks[2]);
            }
        }
    }

    fn on_key(&mut self, kind: KeyEventKind, key: KeyCode, _ctrl: bool) -> Result<Action> {
        match self.mode {
            Mode::Select => match key {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(Action::Quit),
                KeyCode::Up => { if self.selected > 0 { self.selected -= 1; } }
                KeyCode::Down => { if self.selected + 1 < self.cfg.projects.len() { self.selected += 1; } }
                KeyCode::Enter => {
                    let project = self.cfg.projects[self.selected].clone();
                    let log_path = log_path_from_uproject(&project.uproject)?;
                    self.start_tail(project, log_path)?;
                    self.mode = Mode::View;
                }
                _ => {}
            },
            Mode::View => match key {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(Action::Quit),
                KeyCode::Char('c') => { let _ = self.tx_cmd.send(Cmd::Clear); self.lines.clear(); self.scroll_from_bottom = 0; }
                KeyCode::Char('t') if kind == KeyEventKind::Press => { self.show_timestamp = !self.show_timestamp; }
                KeyCode::Char('t') => { /* ignore repeats/releases for toggle */ }
                KeyCode::Char('f') => { self.active_category_filter = None; }
                KeyCode::Char('s') => { 
                    // Return to project selection menu
                    self.mode = Mode::Select; 
                    self.current = None;
                    self.lines.clear();
                    self.scroll_from_bottom = 0;
                    self.last_error = None;
                    self.active_category_filter = None;
                }
                KeyCode::Up => self.scroll_up(1),
                KeyCode::Down => self.scroll_down(1),
                KeyCode::PageUp => self.scroll_up(10),
                KeyCode::PageDown => self.scroll_down(10),
                KeyCode::Home => { self.scroll_from_bottom = self.lines.len(); } // go to top
                KeyCode::End => { self.scroll_from_bottom = 0; } // bottom
                _ => {}
            },
        }
        Ok(Action::Continue)
    }

    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        if self.mode != Mode::View { return; }
        // Only react to left button down
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            // Check click within log body content area (account for borders of block)
            let body = self.last_body_area;
            if m.column >= body.x + 1 && m.column < body.x + body.width - 1 &&
               m.row >= body.y + 1 && m.row < body.y + body.height - 1 {
                // Build filtered list
                let filtered_indices: Vec<usize> = if let Some(cat) = &self.active_category_filter {
                    self.lines.iter().enumerate().filter(|(_, l)| l.category.as_deref() == Some(cat.as_str())).map(|(i, _)| i).collect()
                } else { (0..self.lines.len()).collect() };
                let h = (body.height.saturating_sub(2)) as usize; // content height inside borders
                let total = filtered_indices.len();
                let end = total.saturating_sub(self.scroll_from_bottom);
                let start = end.saturating_sub(h);
                let offset_row = (m.row - (body.y + 1)) as usize;
                let idx_in_view = start + offset_row;
                if idx_in_view < end && idx_in_view < filtered_indices.len() {
                    let line_idx = filtered_indices[idx_in_view];
                    if let Some(cat) = &self.lines[line_idx].category {
                        // Determine x range of category span in content coordinates
                        let ts_len = if self.show_timestamp { self.lines[line_idx].ts.as_ref().map(|t| t.len() + 3).unwrap_or(0) } else { 0 }; // [ts] and trailing space => +3 for [ ] and space
                        let cat_len = cat.len() + 1; // include ':'
                        let cat_start = ts_len;
                        let cat_end = ts_len + cat_len;
                        let content_x = (m.column - (body.x + 1)) as usize;
                        if content_x >= cat_start && content_x < cat_end {
                            self.active_category_filter = Some(cat.clone());
                            self.scroll_from_bottom = 0; // jump to bottom on new filter
                        }
                    }
                }
            }
        }
    }

    fn push_line(&mut self, line: LogLine) {
        self.lines.push(line);
        // cap memory – keep last 20k lines
        const CAP: usize = 20_000;
        if self.lines.len() > CAP {
            let overflow = self.lines.len() - CAP;
            self.lines.drain(0..overflow);
            // avoid jumping when scrolled
            if self.scroll_from_bottom > 0 {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(overflow);
            }
        }
        // autoscroll if pinned to bottom
        // (i.e., scroll_from_bottom == 0 keeps the viewport glued to the end)
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_from_bottom = (self.scroll_from_bottom + n).min(self.lines.len());
    }
    fn scroll_down(&mut self, n: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n);
    }

    fn start_tail(&mut self, project: Project, log_path: PathBuf) -> Result<()> {
        self.current = Some(project.clone());
        self.lines.clear();
        self.scroll_from_bottom = 0;
        self.last_error = Some(format!("Watching: {}", log_path.display()));

        // spawn a new tail thread dedicated to this log path
        let (tx_ev, rx_ev) = mpsc::channel::<AppEvent>();
        let (tx_cmd, rx_cmd) = mpsc::channel::<Cmd>();
        spawn_tail(log_path, tx_ev.clone(), rx_cmd);
        // swap channels into app
        self.rx = rx_ev;
        self.tx_cmd = tx_cmd;
        Ok(())
    }
}

/* ---------------------------- Tail threads --------------------------- */

fn spawn_idle_tail(tx: mpsc::Sender<AppEvent>, rx_cmd: mpsc::Receiver<Cmd>) {
    thread::spawn(move || {
        let _ = rx_cmd.recv(); // block forever until replaced by a real tail
        let _ = tx.send(AppEvent::Tick);
    });
}

fn spawn_tail(path: PathBuf, tx: mpsc::Sender<AppEvent>, rx_cmd: mpsc::Receiver<Cmd>) {
    thread::spawn(move || {
        // Start from EOF; we don't want to flood with old lines.
        let mut offset: u64 = match fs::metadata(&path) { Ok(m) => m.len(), Err(_) => 0 };
        let mut carry = String::new();

        loop {
            // Commands (non-blocking)
            if let Ok(Cmd::Clear) = rx_cmd.try_recv() {
                if let Ok(len) = fs::metadata(&path).map(|m| m.len()) {
                    offset = len;
                }
                carry.clear();
            }

            // Try to read new data
            match fs::metadata(&path) {
                Ok(meta) => {
                    let len = meta.len();
                    if offset > len { offset = 0; } // rotated or truncated

                    if len > offset {
                        let to_read = (len - offset) as usize;
                        if let Ok(mut f) = File::open(&path) {
                            if f.seek(SeekFrom::Start(offset)).is_ok() {
                                let mut buf = vec![0u8; to_read];
                                match f.read(&mut buf) {
                                    Ok(n) if n > 0 => {
                                        offset += n as u64;
                                        let chunk = String::from_utf8_lossy(&buf[..n]);
                                        carry.push_str(&chunk);

                                        // Split on '\n', keep trailing partial in 'carry'
                                        let mut parts = carry.split('\n').map(|s| s.to_string()).collect::<Vec<_>>();
                                        carry = if chunk.ends_with('\n') { String::new() } else { parts.pop().unwrap_or_default() };

                                        for mut line in parts {
                                            if line.ends_with('\r') { let _ = line.pop(); }
                                            if line.trim().is_empty() { continue; }
                                            let color = classify_line(&line);
                                            let (ts, category, message) = parse_log_components(&line);
                                            let _ = tx.send(AppEvent::Line(LogLine { text: line, color, ts, category, message }));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // file not found yet – chill
                }
            }

            thread::sleep(Duration::from_millis(150));
        }
    });
}

/* ------------------------------ Helpers ------------------------------ */

fn load_config() -> Result<Config> {
    // 1) next to the executable
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("projects.json"));
        }
    }

    // 2) current working directory (useful for `cargo run`)
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("projects.json"));
    }

    // 3) project root at compile time
    #[cfg(debug_assertions)]
    {
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("projects.json"));
    }

    let path = candidates.into_iter().find(|p| p.exists())
        .ok_or_else(|| anyhow!("projects.json not found next to the binary nor in current dir"))?;

    let bytes = fs::read(&path).with_context(|| format!("Reading {}", path.display()))?;
    let cfg: Config = serde_json::from_slice(&bytes).with_context(|| format!("Parsing {}", path.display()))?;

    Ok(cfg)
}

fn log_path_from_uproject(uproject: &Path) -> Result<PathBuf> {
    let dir = uproject.parent().ok_or_else(|| anyhow!("Invalid .uproject path"))?;
    let stem = uproject.file_stem().ok_or_else(|| anyhow!("Invalid .uproject filename"))?
        .to_string_lossy().to_string();
    Ok(dir.join("Saved").join("Logs").join(format!("{}.log", stem)))
}

fn classify_line(s: &str) -> Color {
    let l = s.to_ascii_lowercase();
    if l.contains("error") { Color::Red }
    else if l.contains("warning") { Color::Yellow }
    else { Color::White }
}

fn parse_log_components(s: &str) -> (Option<String>, Option<String>, String) {
    // Extract first [timestamp] if present, skip second [thread] if present, then category before ':'
    let mut i = 0usize;
    let bytes = s.as_bytes();
    let mut ts: Option<String> = None;

    // helper to skip spaces
    let mut skip_spaces = |i: usize| -> usize {
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() { j += 1; }
        j
    };

    let mut pos = 0usize;
    if bytes.get(0) == Some(&b'[') {
        if let Some(end) = s.find(']') {
            ts = Some(s[1..end].to_string());
            pos = end + 1;
            pos = skip_spaces(pos);
            // optional second bracket [number]
            if bytes.get(pos) == Some(&b'[') {
                if let Some(end2rel) = s[pos..].find(']') {
                    pos = pos + end2rel + 1;
                    pos = skip_spaces(pos);
                }
            }
        }
    }

    let rest = s[pos..].trim_start();
    // Extract category if like Word: (no spaces before colon)
    let mut category: Option<String> = None;
    let mut message = rest.to_string();
    if let Some(colon_idx) = rest.find(':') {
        let (left, right) = rest.split_at(colon_idx);
        if !left.is_empty() && !left.contains(' ') {
            category = Some(left.to_string());
            message = right.trim_start_matches(':').trim_start().to_string();
        }
    }
    (ts, category, message)
}

trait ListStateExt {
    fn with_selected(self, idx: Option<usize>) -> Self;
}
impl ListStateExt for ratatui::widgets::ListState {
    fn with_selected(mut self, idx: Option<usize>) -> Self { self.select(idx); self }
}

trait ProjectExt {
    fn name_or_key(&self) -> String;
}
impl ProjectExt for Project {
    fn name_or_key(&self) -> String {
        if self.name.trim().is_empty() { self.key.clone() } else { self.name.clone() }
    }
}
