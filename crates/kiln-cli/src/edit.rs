//! File writes for generators: new files and `mod`/`use` lines in module files.

use std::{fs, path::Path};

/// Files a generator will write, checked for conflicts before anything is written.
#[derive(Default)]
pub struct Plan {
    pub files: Vec<(String, String)>,
    /// Existing files rewritten in place (e.g. adding an attribute).
    pub replacements: Vec<(String, String)>,
    pub mod_lines: Vec<(String, String)>,
    /// Text appended to a file (created with `header` if missing).
    pub appends: Vec<(String, String, String)>,
}

impl Plan {
    pub fn file(&mut self, path: impl Into<String>, contents: impl Into<String>) {
        self.files.push((path.into(), contents.into()));
    }

    /// Every path this plan writes, relative to the app root.
    pub fn written(&self) -> impl Iterator<Item = &str> {
        self.files
            .iter()
            .chain(&self.replacements)
            .map(|(p, _)| p.as_str())
            .chain(self.appends.iter().map(|(p, _, _)| p.as_str()))
            .chain(self.mod_lines.iter().map(|(p, _)| p.as_str()))
    }

    pub fn replace(&mut self, path: impl Into<String>, contents: impl Into<String>) {
        self.replacements.push((path.into(), contents.into()));
    }

    pub fn module(&mut self, path: impl Into<String>, line: impl Into<String>) {
        self.mod_lines.push((path.into(), line.into()));
    }

    pub fn append(&mut self, path: impl Into<String>, header: impl Into<String>, text: impl Into<String>) {
        self.appends.push((path.into(), header.into(), text.into()));
    }

    /// Writes everything under `root`. Refuses if a new file already exists, unless `force`.
    pub fn apply(&self, root: &Path, force: bool) -> Result<Vec<String>, String> {
        if !force {
            let existing: Vec<&str> =
                self.files.iter().filter(|(p, _)| root.join(p).exists()).map(|(p, _)| p.as_str()).collect();
            if !existing.is_empty() {
                return Err(format!(
                    "these files already exist (pass --force to overwrite):\n  {}",
                    existing.join("\n  ")
                ));
            }
        }
        let mut log = Vec::new();
        for (path, contents) in &self.files {
            write(&root.join(path), contents)?;
            log.push(format!("create  {path}"));
        }
        for (path, contents) in &self.replacements {
            write(&root.join(path), contents)?;
            log.push(format!("update  {path}"));
        }
        for (path, header, text) in &self.appends {
            let full = root.join(path);
            let current = fs::read_to_string(&full).unwrap_or_else(|_| header.clone());
            // Already there, even if rustfmt reformatted it since: compare the first line.
            let first_line = text.trim().lines().next().unwrap_or_default();
            if current.contains(text.trim()) || current.lines().any(|l| l.trim() == first_line.trim()) {
                continue;
            }
            let mut updated = current.trim_end().to_owned();
            updated.push_str("\n\n");
            updated.push_str(text.trim_end());
            updated.push('\n');
            write(&full, &updated)?;
            log.push(format!("update  {path}"));
        }
        for (path, line) in &self.mod_lines {
            if add_line(&root.join(path), line)? {
                log.push(format!("update  {path}"));
            }
        }
        log.sort();
        log.dedup();
        Ok(log)
    }
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// The kind of a module-file line, for grouping: `pub mod`, `mod`, `pub use`.
fn group(line: &str) -> Option<&'static str> {
    ["pub mod ", "mod ", "pub use "].into_iter().find(|p| line.starts_with(p))
}

/// Adds `line` to a module file, sorted among lines of the same kind. Returns whether the
/// file changed.
pub fn add_line(path: &Path, line: &str) -> Result<bool, String> {
    let current = fs::read_to_string(path).unwrap_or_default();
    if current.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    let mut lines: Vec<String> = current.lines().map(str::to_owned).collect();
    let kind = group(line).ok_or_else(|| format!("not a module line: {line}"))?;
    let same: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| group(l) == Some(kind)).map(|(i, _)| i).collect();
    let at = match same.iter().find(|&&i| lines[i].as_str() > line) {
        Some(&i) => i,
        None => match same.last() {
            Some(&i) => i + 1,
            None => {
                // A new group: `mod` lines before `use` lines, separated by a blank line.
                let last_mod = lines.iter().rposition(|l| l.starts_with("pub mod ") || l.starts_with("mod "));
                match (kind, last_mod) {
                    ("pub use ", Some(i)) => {
                        lines.insert(i + 1, String::new());
                        i + 2
                    }
                    _ => last_mod.map_or(lines.len(), |i| i + 1),
                }
            }
        },
    };
    lines.insert(at, line.to_owned());
    let mut updated = lines.join("\n");
    updated.push('\n');
    write(path, &updated)?;
    Ok(true)
}
