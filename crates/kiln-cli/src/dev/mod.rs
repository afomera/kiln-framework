//! `kiln dev`: builds the app, runs the processes in `Procfile.dev`, and rebuilds and
//! restarts the app's processes when its Rust code changes.
//!
//! ```text
//! # Procfile.dev: `name: command`. $APP is the freshly built app binary.
//! web: $APP serve
//! css: tailwindcss -i assets/app.tailwind.css -o assets/app.css --watch
//! ```
//!
//! Without a `Procfile.dev`, it runs `web: $APP serve`. Processes whose command uses `$APP`
//! restart after every successful rebuild; the rest (asset watchers) keep running. Templates
//! and assets are read from disk on every request in development, so editing them needs no
//! rebuild. A service that crashes is restarted (after a second, up to five times in a row).
//!
//! In a terminal it shows a TUI ([`tui`]); piped, in CI, or with `--inline` it prints
//! prefixed lines ([`inline`]), and with `--json` NDJSON events. However it's stopped (q,
//! Ctrl-C, SIGTERM, SIGHUP, a panic), every process group it started gets SIGTERM, then
//! SIGKILL after a grace period, so nothing keeps holding the port.

mod build;
mod desktop;
mod inline;
mod lines;
mod process;
mod procfile;
mod supervisor;
mod tui;

use std::{
    io::IsTerminal,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use self::{
    build::Diagnostics,
    inline::{Frontend, Inline, Json},
    process::Io,
    supervisor::{Config, Event, Supervisor},
};
use crate::app::App;

/// Signals received (Ctrl-C, SIGTERM, SIGHUP). The first stops every process cleanly;
/// a second kills them.
static STOP: AtomicU32 = AtomicU32::new(0);

/// `kiln dev`'s flags.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub port: Option<u16>,
    /// Prefixed lines instead of the TUI.
    pub inline: bool,
    /// NDJSON events instead of the TUI.
    pub json: bool,
}

/// How `kiln dev` shows what's going on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Tui,
    Inline,
    Json,
}

/// What `kiln dev` is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surroundings {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    /// `CI` is set (and isn't `0` or `false`).
    pub ci: bool,
    /// `TERM=dumb`.
    pub dumb: bool,
    /// The terminal's columns and rows.
    pub size: Option<(u16, u16)>,
}

impl Surroundings {
    fn detect() -> Self {
        let ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"));
        Self {
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty: std::io::stdout().is_terminal(),
            ci,
            dumb: std::env::var("TERM").is_ok_and(|t| t == "dumb"),
            size: ratatui::crossterm::terminal::size().ok(),
        }
    }
}

/// The TUI only for an interactive terminal big enough for it; plain lines otherwise.
pub fn choose(options: &Options, around: &Surroundings) -> Mode {
    if options.json {
        return Mode::Json;
    }
    let big_enough = around.size.is_some_and(|(cols, rows)| cols >= tui_min().0 && rows >= tui_min().1);
    if options.inline || !around.stdin_tty || !around.stdout_tty || around.ci || around.dumb || !big_enough {
        return Mode::Inline;
    }
    Mode::Tui
}

fn tui_min() -> (u16, u16) {
    (70, 15)
}

/// Runs `kiln dev` until it's stopped.
pub fn run(app: &App, options: Options) -> Result<bool, String> {
    let entries = procfile(app)?;
    let port =
        options.port.map(|p| p.to_string()).or_else(|| std::env::var("PORT").ok()).unwrap_or_else(|| "3000".into());
    // However `kiln dev` is stopped (Ctrl-C, an IDE's stop button, `kill`, a closed
    // terminal), it stops what it started, so nothing keeps holding the port.
    let _ = ctrlc::set_handler(|| {
        STOP.fetch_add(1, Ordering::SeqCst);
    });
    install_panic_hook();

    let around = Surroundings::detect();
    let mode = choose(&options, &around);
    let (tx, rx) = mpsc::channel();
    let mut config = Config {
        root: app.root.clone(),
        package: app.package.clone(),
        crate_name: app.crate_name(),
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
        port,
        io: Io::Pipes,
        diagnostics: Diagnostics::Terminal,
        color: true,
        notify: around.stdout_tty && !around.ci,
    };

    if mode == Mode::Tui {
        match tui::Tui::enter(tx.clone()) {
            Ok(mut tui) => {
                let mut sup = Supervisor::new(config.clone(), &entries, tx.clone());
                let (cols, rows) = tui.pane_size(&sup);
                sup.config.io = Io::Pty { cols, rows };
                sup.config.diagnostics = Diagnostics::Captured { color: true, width: cols };
                tui.select_build(&sup);
                sup.begin();
                event_loop(&mut sup, &rx, &mut tui);
                tui.exit(&sup);
                return Ok(true);
            }
            Err(e) => eprintln!("kiln dev: couldn't start the terminal UI ({e}); printing output instead"),
        }
    }
    if mode == Mode::Json {
        config.diagnostics = Diagnostics::Captured { color: false, width: 80 };
        config.color = false;
        config.notify = false;
    }
    let mut sup = Supervisor::new(config, &entries, tx);
    sup.begin();
    if mode == Mode::Json {
        event_loop(&mut sup, &rx, &mut Json);
    } else {
        let mut inline = Inline::new(&sup);
        event_loop(&mut sup, &rx, &mut inline);
    }
    Ok(true)
}

/// Feeds events to the supervisor and the frontend until everything has stopped.
fn event_loop(sup: &mut Supervisor, rx: &Receiver<Event>, frontend: &mut dyn Frontend) {
    let mut signals = 0;
    loop {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
            // Bounded, so a flood of output can't keep the screen from updating.
            for event in std::iter::once(event).chain(rx.try_iter().take(4096)) {
                match event {
                    Event::Input(input) => frontend.input(sup, input),
                    other => sup.handle(other),
                }
            }
        }
        let received = STOP.load(Ordering::SeqCst);
        if received > signals {
            signals = received;
            if received == 1 {
                sup.quit();
            } else {
                sup.force();
            }
        }
        sup.tick(Instant::now());
        frontend.update(sup);
        if sup.done() {
            break;
        }
    }
}

/// On a panic in the main thread: put the terminal back, then stop every process group.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let main = std::thread::current().name() == Some("main");
        if main {
            tui::restore();
        }
        previous(info);
        if main {
            process::terminate_all(supervisor::GRACE);
        }
    }));
}

fn procfile(app: &App) -> Result<Vec<procfile::Entry>, String> {
    let path = app.path("Procfile.dev");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => procfile::DEFAULT.to_owned(),
        Err(e) => return Err(format!("couldn't read {}: {e}", path.display())),
    };
    procfile::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TERMINAL: Surroundings =
        Surroundings { stdin_tty: true, stdout_tty: true, ci: false, dumb: false, size: Some((120, 40)) };

    #[test]
    fn tui_only_in_a_real_terminal() {
        let options = Options::default();
        assert_eq!(choose(&options, &TERMINAL), Mode::Tui);
        assert_eq!(choose(&options, &Surroundings { stdout_tty: false, ..TERMINAL }), Mode::Inline);
        assert_eq!(choose(&options, &Surroundings { stdin_tty: false, ..TERMINAL }), Mode::Inline);
        assert_eq!(choose(&options, &Surroundings { ci: true, ..TERMINAL }), Mode::Inline);
        assert_eq!(choose(&options, &Surroundings { dumb: true, ..TERMINAL }), Mode::Inline);
        assert_eq!(choose(&options, &Surroundings { size: Some((50, 40)), ..TERMINAL }), Mode::Inline);
        assert_eq!(choose(&options, &Surroundings { size: None, ..TERMINAL }), Mode::Inline);
    }

    #[test]
    fn flags_override() {
        let inline = Options { inline: true, ..Options::default() };
        assert_eq!(choose(&inline, &TERMINAL), Mode::Inline);
        let json = Options { json: true, ..Options::default() };
        assert_eq!(choose(&json, &TERMINAL), Mode::Json);
        assert_eq!(choose(&json, &Surroundings { stdout_tty: false, ci: true, ..TERMINAL }), Mode::Json);
    }
}
