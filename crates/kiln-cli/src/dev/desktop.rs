//! Best-effort desktop integration: notifications and opening the browser.

use std::{
    process::{Command, Stdio},
    thread,
};

/// Shows a desktop notification (macOS: `osascript`, elsewhere `notify-send`), if possible.
pub fn notify(title: &str, message: &str) {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut command = if cfg!(target_os = "macos") {
        let mut c = Command::new("osascript");
        c.arg("-e").arg(format!("display notification {} with title {}", quote(message), quote(title)));
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args([title, message]);
        c
    };
    run_detached(&mut command);
}

/// Opens `url` in the default browser, if possible.
pub fn open(url: &str) {
    let mut command = Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    command.arg(url);
    run_detached(&mut command);
}

fn run_detached(command: &mut Command) {
    let spawned = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    if let Ok(mut child) = spawned {
        // Reap it so it doesn't linger as a zombie.
        thread::spawn(move || child.wait());
    }
}
