//! The terminal UI: a sidebar of processes with their status, the selected process's output
//! (or every process's, interleaved, in the stream view), search, filtering and keys to
//! restart, stop and start processes.

mod ansi;
mod render;

use std::{
    io::{self, Stdout, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        cursor,
        event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        execute,
        terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    },
};

use super::{
    desktop,
    inline::{COLORS, Frontend},
    lines::Line,
    supervisor::{Event, Notice, Supervisor},
};

/// Whether the terminal is in raw mode on the alternate screen (for the panic hook).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Lines of output printed to the normal screen on exit.
const FLUSH_LINES: usize = 500;

/// Puts the terminal back the way it was, if the TUI changed it.
pub fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

/// Which output is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The selected process's.
    Tabs,
    /// Every process's, interleaved.
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptKind {
    Search,
    Filter,
}

/// Scroll position of one output view.
#[derive(Debug, Clone, Default)]
struct Scroll {
    /// Lines between the bottom line shown and the newest; 0 follows new output.
    offset: usize,
    /// Lines ever pushed when last looked at, to keep the view still as more arrive.
    seen: u64,
    /// New output arrived below while scrolled up.
    unseen: bool,
    /// The bottom line drawn last, as an index into the view's lines.
    bottom: usize,
    /// The current search match.
    cursor: Option<usize>,
}

/// What the TUI shows and where.
struct Ui {
    selected: usize,
    /// The user picked a process (so the first build doesn't move the selection).
    chosen: bool,
    view: View,
    scrolls: Vec<Scroll>,
    stream: Scroll,
    prompt: Option<(PromptKind, String)>,
    search: Option<String>,
    filter: Option<String>,
    flash: Option<(String, Instant)>,
    /// Ctrl-C / q presses: the first quits, the next skips the grace period.
    quits: u32,
    /// Output rows visible in the pane, from the last draw.
    height: usize,
    /// The pane's size, which the processes' terminals match.
    pane: (u16, u16),
}

/// The running TUI. Dropping it restores the terminal.
pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    ui: Ui,
    dirty: bool,
    last_draw: Instant,
}

impl Tui {
    /// Switches the terminal to the TUI and starts reading keys into `events`.
    pub fn enter(events: Sender<Event>) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, cursor::Hide) {
            restore();
            return Err(e);
        }
        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(e) => {
                restore();
                return Err(e);
            }
        };
        thread::spawn(move || {
            while ACTIVE.load(Ordering::SeqCst) {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(e) => {
                            if events.send(Event::Input(e)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let ui = Ui {
            selected: 0,
            chosen: false,
            view: View::Tabs,
            scrolls: vec![],
            stream: Scroll::default(),
            prompt: None,
            search: None,
            filter: None,
            flash: None,
            quits: 0,
            height: rows.saturating_sub(3) as usize,
            pane: render::pane_size(cols, rows, &[]),
        };
        Ok(Self { terminal, ui, dirty: true, last_draw: Instant::now() - Duration::from_secs(1) })
    }

    /// The size processes' terminals should have, before anything is drawn.
    pub fn pane_size(&self, sup: &Supervisor) -> (u16, u16) {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        render::pane_size(cols, rows, &sup.procs)
    }

    /// Shows the build first, until the app is up.
    pub fn select_build(&mut self, sup: &Supervisor) {
        self.ui.selected = sup.build;
    }

    /// Leaves the TUI and prints recent output to the normal screen so it isn't lost.
    pub fn exit(self, sup: &Supervisor) {
        drop(self);
        let width = sup.procs.iter().map(|p| p.name.len()).max().unwrap_or(3).max(3);
        let mut out = io::stdout().lock();
        let skip = sup.combined.len().saturating_sub(FLUSH_LINES);
        if skip > 0 {
            let _ = writeln!(out, "\x1b[2m… {skip} earlier lines not shown\x1b[0m");
        }
        for (id, line) in sup.combined.iter().skip(skip) {
            let color = COLORS[*id % COLORS.len()];
            let name = &sup.procs[*id].name;
            if line.system {
                let _ = writeln!(out, "\x1b[{color}m{name:>width$} |\x1b[0m \x1b[2m{}\x1b[0m", line.text);
            } else {
                let _ = writeln!(out, "\x1b[{color}m{name:>width$} |\x1b[0m {}\x1b[0m", line.text);
            }
        }
        let _ = out.flush();
    }

    fn draw(&mut self, sup: &Supervisor) {
        let ui = &mut self.ui;
        // A hung-up terminal can't be drawn on; shutting down matters more.
        let _ = self.terminal.draw(|frame| render::draw(frame, ui, sup));
        self.last_draw = Instant::now();
        self.dirty = false;
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore();
    }
}

impl Frontend for Tui {
    fn input(&mut self, sup: &mut Supervisor, event: TermEvent) {
        self.dirty = true;
        match event {
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => self.ui.key(sup, key),
            TermEvent::Resize(..) => {
                let _ = self.terminal.autoresize();
            }
            _ => {}
        }
    }

    fn update(&mut self, sup: &mut Supervisor) {
        for notice in std::mem::take(&mut sup.notices) {
            self.dirty = true;
            if let Notice::BuildSucceeded { first: true } = notice
                && !self.ui.chosen
            {
                self.ui.selected = 0;
            }
        }
        self.ui.follow(sup);
        let now = Instant::now();
        let since = now.duration_since(self.last_draw);
        // Redraw on changes (at most ~30 times a second), and a few times a second anyway
        // for spinners and the clock-driven states (restarting, stopping).
        if (self.dirty && since >= Duration::from_millis(33)) || since >= Duration::from_millis(250) {
            self.draw(sup);
            let size = render::pane_size_for(&self.ui, &sup.procs, self.terminal.size().ok());
            if size != self.ui.pane {
                self.ui.pane = size;
                sup.resize(size.0, size.1);
            }
        }
    }
}

impl Ui {
    fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    fn scroll_mut(&mut self) -> &mut Scroll {
        match self.view {
            View::Stream => &mut self.stream,
            View::Tabs => &mut self.scrolls[self.selected],
        }
    }

    /// Keeps scrolled-up views still as output arrives below them.
    fn follow(&mut self, sup: &Supervisor) {
        self.scrolls.resize_with(sup.procs.len(), Scroll::default);
        self.selected = self.selected.min(sup.procs.len() - 1);
        let filter = self.filter.clone();
        for (scroll, proc) in self.scrolls.iter_mut().zip(&sup.procs) {
            let arrived = (proc.lines.total() - scroll.seen) as usize;
            scroll.seen = proc.lines.total();
            let lines = proc.lines.iter().rev().take(arrived).map(|l| &**l);
            nudge(scroll, count(lines, filter.as_deref()));
        }
        let arrived = (sup.combined.total() - self.stream.seen) as usize;
        self.stream.seen = sup.combined.total();
        let lines = sup.combined.iter().rev().take(arrived).map(|(_, l)| &**l);
        nudge(&mut self.stream, count(lines, filter.as_deref()));
    }

    /// The lines in the current view (after the filter), oldest first.
    fn lines<'a>(&self, sup: &'a Supervisor) -> Vec<(usize, &'a Line)> {
        let all: Box<dyn Iterator<Item = (usize, &Line)>> = match self.view {
            View::Tabs => Box::new(sup.procs[self.selected].lines.iter().map(|l| (self.selected, &**l))),
            View::Stream => Box::new(sup.combined.iter().map(|(id, l)| (*id, &**l))),
        };
        match self.filter.as_deref() {
            Some(filter) => all.filter(|(_, l)| ansi::contains(&l.plain, filter)).collect(),
            None => all.collect(),
        }
    }

    fn key(&mut self, sup: &mut Supervisor, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.quit(sup);
        }
        if let Some((kind, text)) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let (kind, text) = (*kind, text.trim().to_ascii_lowercase());
                    self.prompt = None;
                    let text = (!text.is_empty()).then_some(text);
                    match kind {
                        PromptKind::Search => {
                            self.search = text;
                            self.scroll_mut().cursor = None;
                            if self.search.is_some() {
                                self.next_match(sup, true);
                            }
                        }
                        PromptKind::Filter => {
                            self.filter = text;
                            for scroll in self.scrolls.iter_mut().chain([&mut self.stream]) {
                                *scroll = Scroll { seen: scroll.seen, ..Scroll::default() };
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !ctrl => text.push(c),
                _ => {}
            }
            return;
        }
        if sup.quitting() {
            if key.code == KeyCode::Char('q') {
                self.quit(sup);
            }
            return;
        }
        let count = sup.procs.len();
        let page = self.height.saturating_sub(1).max(1);
        match key.code {
            KeyCode::Char('q') => self.quit(sup),
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < count {
                    self.select(index);
                }
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Right => self.select((self.selected + 1) % count),
            KeyCode::BackTab | KeyCode::Up | KeyCode::Left => self.select((self.selected + count - 1) % count),
            KeyCode::Char('j') => self.scroll(sup, -1),
            KeyCode::Char('k') => self.scroll(sup, 1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll(sup, -(page as isize)),
            KeyCode::PageUp => self.scroll(sup, page as isize),
            KeyCode::Char('d') if ctrl => self.scroll(sup, -(page as isize / 2)),
            KeyCode::Char('u') if ctrl => self.scroll(sup, page as isize / 2),
            KeyCode::Char('g') | KeyCode::Home => {
                let len = self.lines(sup).len();
                let scroll = self.scroll_mut();
                scroll.offset = len.saturating_sub(1);
                scroll.cursor = None;
            }
            KeyCode::Char('G') | KeyCode::End => {
                let scroll = self.scroll_mut();
                scroll.offset = 0;
                scroll.unseen = false;
                scroll.cursor = None;
            }
            KeyCode::Char('r') => {
                sup.restart(self.selected);
                self.flash(format!("restarting {}", sup.procs[self.selected].name));
            }
            KeyCode::Char('s') => sup.toggle(self.selected),
            KeyCode::Char('c') => match self.view {
                View::Tabs => sup.clear(self.selected),
                View::Stream => sup.combined.clear(),
            },
            KeyCode::Char('m') => {
                self.view = if self.view == View::Tabs { View::Stream } else { View::Tabs };
            }
            KeyCode::Char('/') => self.prompt = Some((PromptKind::Search, String::new())),
            KeyCode::Char('f') => self.prompt = Some((PromptKind::Filter, self.filter.clone().unwrap_or_default())),
            KeyCode::Char('n') => self.next_match(sup, true),
            KeyCode::Char('N') => self.next_match(sup, false),
            KeyCode::Esc => {
                self.search = None;
                self.filter = None;
                self.scroll_mut().cursor = None;
            }
            KeyCode::Char('o') => {
                let url = format!("{}/kiln", sup.url());
                desktop::open(&url);
                self.flash(format!("opening {url}"));
            }
            KeyCode::Char('K') => {
                let id = sup.task("check", &["check"]);
                self.select(id);
                self.view = View::Tabs;
            }
            KeyCode::Char('T') => {
                let id = sup.task("test", &["test"]);
                self.select(id);
                self.view = View::Tabs;
            }
            _ => {}
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        self.chosen = true;
        if self.scrolls.len() <= index {
            self.scrolls.resize_with(index + 1, Scroll::default);
        }
    }

    fn quit(&mut self, sup: &mut Supervisor) {
        self.quits += 1;
        if self.quits == 1 {
            sup.quit();
            self.flash("stopping… (press q or Ctrl-C again to kill)");
        } else {
            sup.force();
        }
    }

    /// Scrolls `by` lines (positive: up, to older output).
    fn scroll(&mut self, sup: &Supervisor, by: isize) {
        let len = self.lines(sup).len();
        let scroll = self.scroll_mut();
        // Start from what's on screen: near the top, the view shows more than `offset` says.
        let shown = len.saturating_sub(1).saturating_sub(scroll.bottom.min(len.saturating_sub(1)));
        let offset = if scroll.offset == 0 { 0 } else { shown };
        scroll.offset = offset.saturating_add_signed(by).min(len.saturating_sub(1));
        scroll.cursor = None;
        if scroll.offset == 0 {
            scroll.unseen = false;
        }
    }

    /// Jumps to the next search match, older (`up`) or newer.
    fn next_match(&mut self, sup: &Supervisor, up: bool) {
        let Some(query) = self.search.clone() else { return };
        let lines = self.lines(sup);
        let height = self.height;
        let scroll = self.scroll_mut();
        let len = lines.len();
        let from = scroll.cursor.unwrap_or(if scroll.offset == 0 { len } else { scroll.bottom + 1 });
        let hit = |i: &usize| ansi::contains(&lines[*i].1.plain, &query);
        let found = if up { (0..from.min(len)).rev().find(hit) } else { (from + 1..len).find(hit) };
        match found {
            Some(index) => {
                scroll.cursor = Some(index);
                let bottom = (index + height / 2).min(len - 1);
                scroll.offset = len - 1 - bottom;
                if scroll.offset == 0 {
                    scroll.unseen = false;
                }
            }
            None => self.flash(format!("no {} matches for “{query}”", if up { "earlier" } else { "later" })),
        }
    }
}

/// How many of `lines` pass `filter`.
fn count<'a>(lines: impl Iterator<Item = &'a Line>, filter: Option<&str>) -> usize {
    match filter {
        Some(filter) => lines.filter(|l| ansi::contains(&l.plain, filter)).count(),
        None => lines.count(),
    }
}

/// `arrived` new lines: a scrolled-up view moves up with its lines.
fn nudge(scroll: &mut Scroll, arrived: usize) {
    if arrived > 0 && scroll.offset > 0 {
        scroll.offset += arrived;
        scroll.unseen = true;
    }
}
