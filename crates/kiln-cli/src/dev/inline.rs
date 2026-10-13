//! The plain frontends: prefixed lines (`web | ...`, foreman-style) for terminals without
//! the TUI, pipes and CI, and NDJSON events (`--json`) for tools.

use std::io::Write;

use serde_json::json;

use super::supervisor::{Kind, Notice, Supervisor};

/// Colors for process names, in `Procfile.dev` order.
pub const COLORS: [&str; 6] = ["36", "35", "33", "32", "34", "31"];

/// Something that shows what the supervisor reports.
pub trait Frontend {
    /// A key press or resize (the TUI's).
    fn input(&mut self, _sup: &mut Supervisor, _event: ratatui::crossterm::event::Event) {}

    /// Shows what happened since the last call (`sup.notices`).
    fn update(&mut self, sup: &mut Supervisor);
}

/// `name | line` for every line, and `dev | message` for kiln dev's own messages.
pub struct Inline {
    width: usize,
}

impl Inline {
    /// Prefixes padded to the longest service name in `sup`.
    pub fn new(sup: &Supervisor) -> Self {
        let width = sup
            .procs
            .iter()
            .filter(|p| matches!(p.kind, Kind::Service { .. }))
            .map(|p| p.name.len())
            .max()
            .unwrap_or(3)
            .max(3);
        Self { width }
    }

    fn say(&self, out: &mut impl Write, message: &str) {
        let _ = writeln!(out, "\x1b[1m{:>width$} |\x1b[0m {message}", "dev", width = self.width);
    }
}

impl Frontend for Inline {
    fn update(&mut self, sup: &mut Supervisor) {
        if sup.notices.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        for notice in std::mem::take(&mut sup.notices) {
            match notice {
                Notice::Output { id, raw, .. } => {
                    let proc = &sup.procs[id];
                    let color = COLORS[id % COLORS.len()];
                    if id == sup.build {
                        let _ = writeln!(out, "{raw}");
                    } else {
                        let _ = writeln!(out, "\x1b[{color}m{:>width$} |\x1b[0m {raw}", proc.name, width = self.width);
                    }
                }
                Notice::Exited { id, exit, requested: false } if id != sup.build => {
                    self.say(&mut out, &format!("{} exited ({exit})", sup.procs[id].name));
                }
                Notice::Restarting { id, attempt, delay } => {
                    let name = &sup.procs[id].name;
                    self.say(&mut out, &format!("{name} restarting in {}s (attempt {attempt})", delay.as_secs_f32()));
                }
                Notice::Failed { id, reason } => {
                    self.say(&mut out, &format!("{} not restarting: {reason}", sup.procs[id].name));
                }
                Notice::Say(message) => self.say(&mut out, &message),
                _ => {}
            }
        }
        let _ = out.flush();
    }
}

/// One JSON object per line: `{"event":"output","process":"web","stream":"stdout","line":".."}`.
pub struct Json;

impl Frontend for Json {
    fn update(&mut self, sup: &mut Supervisor) {
        if sup.notices.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        for notice in std::mem::take(&mut sup.notices) {
            let name = |id: usize| sup.procs[id].name.as_str();
            let event = match notice {
                Notice::Output { id, stream, line, .. } => {
                    json!({"event": "output", "process": name(id), "stream": stream.name(), "line": line.plain})
                }
                Notice::Started { id, pid } => {
                    json!({"event": "start", "process": name(id), "pid": pid, "command": sup.procs[id].command()})
                }
                Notice::Exited { id, exit, requested } => json!({
                    "event": "exit", "process": name(id), "code": exit.code, "signal": exit.signal,
                    "requested": requested,
                }),
                Notice::Restarting { id, attempt, delay } => json!({
                    "event": "restarting", "process": name(id), "attempt": attempt,
                    "delay_ms": delay.as_millis() as u64,
                }),
                Notice::Failed { id, reason } => json!({"event": "failed", "process": name(id), "reason": reason}),
                Notice::Say(message) => json!({"event": "message", "message": message}),
                Notice::BuildStarted => json!({"event": "build", "status": "started"}),
                Notice::BuildSucceeded { first } => {
                    if first {
                        let _ = writeln!(out, "{}", json!({"event": "build", "status": "succeeded"}));
                        json!({"event": "ready", "url": sup.url()})
                    } else {
                        json!({"event": "build", "status": "succeeded"})
                    }
                }
                Notice::BuildFailed => json!({"event": "build", "status": "failed"}),
            };
            let _ = writeln!(out, "{event}");
        }
        let _ = out.flush();
    }
}
