//! Building the app (`cargo build`) and noticing when its sources change.

use std::{
    collections::BTreeSet,
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

/// What a rebuild depends on: the modification times of the Rust sources, and which
/// templates exist. Views and mail check their templates (and the default layout) when
/// they compile, so adding, removing or renaming one needs a build; editing one doesn't
/// (debug builds read templates from disk, and the page live-reloads).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    sources: Vec<(PathBuf, SystemTime)>,
    templates: BTreeSet<PathBuf>,
}

/// Why the app needs building again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A file under src/, build.rs or Cargo.toml changed.
    Sources,
    /// A template was added, removed or renamed.
    Templates,
}

impl Change {
    pub fn describe(self) -> &'static str {
        match self {
            Change::Sources => "change detected; rebuilding",
            Change::Templates => "templates added or removed; rebuilding (views check them when they compile)",
        }
    }
}

/// The current [`Snapshot`] of the app at `root`.
pub fn snapshot(root: &Path) -> Snapshot {
    let mut sources = vec![];
    for entry in ["src", "build.rs", "Cargo.toml"] {
        collect(&root.join(entry), &mut sources);
    }
    sources.sort();
    let templates_dir = root.join("templates");
    let mut files = vec![];
    collect(&templates_dir, &mut files);
    let templates = files
        .into_iter()
        .filter_map(|(path, _)| path.strip_prefix(&templates_dir).ok().map(Path::to_path_buf))
        // Only templates count, not an editor's swap and backup files beside them.
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            name.ends_with(".jinja") && !name.starts_with('.')
        })
        .collect();
    Snapshot { sources, templates }
}

/// What changed between two snapshots that needs a build, if anything.
pub fn change(before: &Snapshot, after: &Snapshot) -> Option<Change> {
    if before.sources != after.sources {
        Some(Change::Sources)
    } else if before.templates != after.templates {
        Some(Change::Templates)
    } else {
        None
    }
}

/// Makes cargo compile the app crate again although no Rust file changed, so its views see
/// the templates as they are now: marks the crate root as modified (its contents stay).
pub fn invalidate(root: &Path) {
    for crate_root in ["src/lib.rs", "src/main.rs"] {
        let path = root.join(crate_root);
        if let Ok(file) = std::fs::OpenOptions::new().append(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
            return;
        }
    }
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
    fn rebuilds_for_sources_and_for_templates_coming_and_going() {
        let root = std::env::temp_dir().join(format!("kiln-dev-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let later = |path: &str| {
            let file = std::fs::OpenOptions::new().append(true).open(root.join(path)).unwrap();
            file.set_modified(SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
        };
        write("src/lib.rs", "pub mod views;");
        write("Cargo.toml", "[package]");
        write("templates/posts/show.html.jinja", "<h1>{{ post.title }}</h1>");
        let start = snapshot(&root);

        // Editing a template live-reloads; it doesn't need a build.
        write("templates/posts/show.html.jinja", "<h2>{{ post.title }}</h2>");
        later("templates/posts/show.html.jinja");
        assert_eq!(change(&start, &snapshot(&root)), None);
        // Nor do an editor's swap and backup files.
        write("templates/posts/.show.html.jinja.swp", "");
        write("templates/posts/show.html.jinja~", "");
        assert_eq!(change(&start, &snapshot(&root)), None);

        // A new template does: a view may name it.
        write("templates/posts/index.html.jinja", "");
        let added = snapshot(&root);
        assert_eq!(change(&start, &added), Some(Change::Templates));
        // So do removing and renaming one.
        std::fs::rename(root.join("templates/posts/index.html.jinja"), root.join("templates/posts/list.html.jinja"))
            .unwrap();
        let renamed = snapshot(&root);
        assert_eq!(change(&added, &renamed), Some(Change::Templates));
        std::fs::remove_file(root.join("templates/posts/list.html.jinja")).unwrap();
        assert_eq!(change(&renamed, &snapshot(&root)), Some(Change::Templates));

        // Rust sources, by modification time.
        later("src/lib.rs");
        assert_eq!(change(&start, &snapshot(&root)), Some(Change::Sources));
        write("src/views/mod.rs", "");
        let before = snapshot(&root);
        // Invalidating marks the crate root modified without changing it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        invalidate(&root);
        assert_eq!(std::fs::read_to_string(root.join("src/lib.rs")).unwrap(), "pub mod views;");
        assert_eq!(change(&before, &snapshot(&root)), Some(Change::Sources));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_cargo_activity() {
        assert_eq!(cargo_activity("   Compiling blog v0.1.0"), Some(Activity::Compiling));
        assert_eq!(cargo_activity("error[E0308]: mismatched types"), Some(Activity::Failed));
        assert_eq!(cargo_activity("    Finished `dev` profile"), Some(Activity::Done));
        assert_eq!(cargo_activity("GET /posts 200"), None);
    }
}
