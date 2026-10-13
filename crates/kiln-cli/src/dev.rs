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
//! rebuild.

use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime},
};

use crate::app::App;

const COLORS: [&str; 6] = ["36", "35", "33", "32", "34", "31"];

struct Proc {
    name: String,
    command: String,
    color: &'static str,
    child: Option<Child>,
}

impl Proc {
    fn uses_app(&self) -> bool {
        self.command.contains("$APP") || self.command.contains("${APP}")
    }
}

pub fn run(app: &App, port: Option<u16>) -> Result<bool, String> {
    let mut procs = procfile(app)?;
    let width = procs.iter().map(|p| p.name.len()).max().unwrap_or(3).max(3);
    let port = port.map(|p| p.to_string()).or_else(|| std::env::var("PORT").ok()).unwrap_or_else(|| "3000".into());
    let say = |message: &str| println!("\x1b[1m{:>width$} |\x1b[0m {message}", "dev");

    // The first build: wait for a fix if it fails.
    let mut seen = snapshot(app);
    let mut binary = loop {
        if let Some(binary) = build(app)? {
            break binary;
        }
        say("build failed; waiting for changes");
        seen = wait_for_change(app, &seen);
    };
    say(&format!("http://127.0.0.1:{port}  (watching src/ and Cargo.toml)"));
    for proc in &mut procs {
        start(proc, &binary, &port, width)?;
    }

    loop {
        thread::sleep(Duration::from_millis(300));
        for proc in &mut procs {
            if let Some(child) = &mut proc.child
                && let Ok(Some(status)) = child.try_wait()
            {
                say(&format!("{} exited ({status})", proc.name));
                proc.child = None;
            }
        }
        let now = snapshot(app);
        if now == seen {
            continue;
        }
        // Let editors finish writing (format-on-save writes twice).
        thread::sleep(Duration::from_millis(150));
        seen = snapshot(app);
        say("change detected; rebuilding");
        match build(app)? {
            Some(rebuilt) => {
                binary = rebuilt;
                for proc in procs.iter_mut().filter(|p| p.uses_app()) {
                    stop(proc);
                    start(proc, &binary, &port, width)?;
                }
                say("restarted");
            }
            None => say("build failed; still running the last good build"),
        }
    }
}

fn procfile(app: &App) -> Result<Vec<Proc>, String> {
    let path = app.path("Procfile.dev");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "web: $APP serve".to_owned(),
        Err(e) => return Err(format!("couldn't read {}: {e}", path.display())),
    };
    let procs: Vec<Proc> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .enumerate()
        .map(|(i, line)| {
            let (name, command) = line
                .split_once(':')
                .ok_or_else(|| format!("Procfile.dev line `{line}` should be `name: command`"))?;
            Ok(Proc {
                name: name.trim().to_owned(),
                command: command.trim().to_owned(),
                color: COLORS[i % COLORS.len()],
                child: None,
            })
        })
        .collect::<Result<_, String>>()?;
    if procs.is_empty() {
        return Err("Procfile.dev has no processes".into());
    }
    Ok(procs)
}

/// `cargo build`, with diagnostics on the terminal. The app binary's path, or `None` if the
/// build failed.
fn build(app: &App) -> Result<Option<PathBuf>, String> {
    let mut child = Command::new("cargo")
        .args(["build", "--message-format", "json-render-diagnostics", "--manifest-path"])
        .arg(app.path("Cargo.toml"))
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    let mut out = String::new();
    child.stdout.take().expect("piped").read_to_string(&mut out).map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Ok(None);
    }
    let binary = out.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find_map(|m| {
        let is_app = m["reason"] == "compiler-artifact"
            && m["target"]["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin"))
            && (m["target"]["name"] == app.package.as_str() || m["target"]["name"] == app.crate_name().as_str());
        is_app.then(|| m["executable"].as_str().map(PathBuf::from)).flatten()
    });
    binary.map(Some).ok_or_else(|| format!("the build has no `{}` binary (src/main.rs)", app.package))
}

fn start(proc: &mut Proc, binary: &Path, port: &str, width: usize) -> Result<(), String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&proc.command)
        .env("APP", binary)
        .env("PORT", port)
        .env("CLICOLOR_FORCE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", proc.name))?;
    let prefix = format!("\x1b[{}m{:>width$} |\x1b[0m ", proc.color, proc.name);
    pipe(child.stdout.take().expect("piped"), prefix.clone());
    pipe(child.stderr.take().expect("piped"), prefix);
    proc.child = Some(child);
    Ok(())
}

fn pipe(output: impl Read + Send + 'static, prefix: String) {
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            println!("{prefix}{line}");
        }
    });
}

/// SIGTERM to the shell and anything it started (shells usually exec a lone command, but
/// not always), then SIGKILL if it hasn't exited after five seconds.
fn stop(proc: &mut Proc) {
    let Some(mut child) = proc.child.take() else { return };
    let pid = child.id().to_string();
    let _ = Command::new("pkill").args(["-TERM", "-P", &pid]).status();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
    for _ in 0..50 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Modification times of everything a rebuild depends on.
fn snapshot(app: &App) -> Vec<(PathBuf, SystemTime)> {
    let mut files = vec![];
    for entry in ["src", "build.rs", "Cargo.toml"] {
        collect(&app.path(entry), &mut files);
    }
    files.sort();
    files
}

fn collect(path: &Path, files: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            collect(&entry.path(), files);
        }
    } else if let Ok(modified) = meta.modified() {
        files.push((path.to_owned(), modified));
    }
}

fn wait_for_change(app: &App, seen: &[(PathBuf, SystemTime)]) -> Vec<(PathBuf, SystemTime)> {
    loop {
        thread::sleep(Duration::from_millis(300));
        let now = snapshot(app);
        if now != seen {
            thread::sleep(Duration::from_millis(150));
            return snapshot(app);
        }
    }
}
