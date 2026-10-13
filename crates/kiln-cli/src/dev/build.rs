//! Building the app (`cargo build`) and noticing when its sources change.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::Sender,
    thread,
    time::SystemTime,
};

use super::{
    process::Child,
    supervisor::{Event, Stream},
};

/// How the build's diagnostics are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnostics {
    /// Straight to the terminal (inline mode).
    Terminal,
    /// Captured as output of the `build` process; `color` keeps cargo's colors and
    /// progress bar (which is `width` columns wide).
    Captured { color: bool, width: u16 },
}

/// Starts `cargo build` for `manifest` in its own process group. Its stdout (cargo's JSON
/// messages) arrives as [`Event::BuildMessages`] tagged `generation`.
pub fn start(
    manifest: &Path,
    diagnostics: Diagnostics,
    id: usize,
    generation: u64,
    events: &Sender<Event>,
) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("cargo");
    command
        .args(["build", "--message-format", "json-render-diagnostics", "--manifest-path"])
        .arg(manifest)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    match diagnostics {
        Diagnostics::Terminal => {
            command.stderr(Stdio::inherit());
        }
        Diagnostics::Captured { color, width } => {
            command.stderr(Stdio::piped());
            if color {
                command
                    .env("CARGO_TERM_COLOR", "always")
                    .env("CARGO_TERM_PROGRESS_WHEN", "always")
                    .env("CARGO_TERM_PROGRESS_WIDTH", width.max(20).to_string());
            } else {
                command.env("CARGO_TERM_COLOR", "never");
            }
        }
    }
    let mut child = command.spawn().map_err(|e| format!("couldn't run cargo: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped");
    if let Some(mut stderr) = child.stderr.take() {
        let events = events.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0
                    || events.send(Event::Output { id, stream: Stream::Stderr, bytes: buf[..n].to_vec() }).is_err()
                {
                    break;
                }
            }
            let _ = events.send(Event::Closed { id, stream: Stream::Stderr });
        });
    }
    let events = events.clone();
    thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = events.send(Event::BuildMessages { generation, stdout: out });
    });
    Ok(Child::from_std(child))
}

/// The app binary from cargo's JSON messages.
pub fn binary(messages: &str, package: &str, crate_name: &str) -> Result<PathBuf, String> {
    messages
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|m| {
            let is_app = m["reason"] == "compiler-artifact"
                && m["target"]["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin"))
                && (m["target"]["name"] == package || m["target"]["name"] == crate_name);
            is_app.then(|| m["executable"].as_str().map(PathBuf::from)).flatten()
        })
        .ok_or_else(|| format!("the build has no `{package}` binary (src/main.rs)"))
}

/// Modification times of everything a rebuild depends on.
pub type Snapshot = Vec<(PathBuf, SystemTime)>;

/// The current [`Snapshot`] of the app at `root`.
pub fn snapshot(root: &Path) -> Snapshot {
    let mut files = vec![];
    for entry in ["src", "build.rs", "Cargo.toml"] {
        collect(&root.join(entry), &mut files);
    }
    files.sort();
    files
}

fn collect(path: &Path, files: &mut Snapshot) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            collect(&entry.path(), files);
        }
    } else if let Ok(modified) = meta.modified() {
        files.push((path.to_owned(), modified));
    }
}

/// What a line of a process's output says about a build it runs (`cargo watch`, `cargo
/// run`): `Some(true)` compiling, `Some(false)` failed, `None` nothing.
pub fn cargo_activity(plain: &str) -> Option<Activity> {
    let line = plain.trim_start();
    if line.starts_with("Compiling ") || line.starts_with("Building [") {
        Some(Activity::Compiling)
    } else if line.starts_with("error[") || line.starts_with("error: could not compile") {
        Some(Activity::Failed)
    } else if line.starts_with("Finished ") || line.starts_with("Running `") {
        Some(Activity::Done)
    } else {
        None
    }
}

/// A build seen in a process's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Compiling,
    Failed,
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_app_binary() {
        let messages = [
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"blog"},"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"other"},"executable":"/t/other"}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"my_app"},"executable":"/t/my-app"}"#,
            "not json",
        ]
        .join("\n");
        assert_eq!(binary(&messages, "my-app", "my_app").unwrap(), PathBuf::from("/t/my-app"));
        assert!(binary("", "blog", "blog").unwrap_err().contains("no `blog` binary"));
    }

    #[test]
    fn reads_cargo_activity() {
        assert_eq!(cargo_activity("   Compiling blog v0.1.0"), Some(Activity::Compiling));
        assert_eq!(cargo_activity("error[E0308]: mismatched types"), Some(Activity::Failed));
        assert_eq!(cargo_activity("    Finished `dev` profile"), Some(Activity::Done));
        assert_eq!(cargo_activity("GET /posts 200"), None);
    }
}
