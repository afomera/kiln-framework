//! `Procfile.dev`: `name: command` lines, `#` comments.

/// One process from `Procfile.dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The label, e.g. `web`.
    pub name: String,
    /// A shell command; `$APP` is the app binary.
    pub command: String,
}

impl Entry {
    /// Whether the command runs the app binary, so it restarts after each rebuild.
    pub fn uses_app(&self) -> bool {
        self.command.contains("$APP") || self.command.contains("${APP}")
    }
}

/// What runs without a `Procfile.dev`.
pub const DEFAULT: &str = "web: $APP serve";

/// The processes in `text`.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (name, command) =
            line.split_once(':').ok_or_else(|| format!("Procfile.dev line `{line}` should be `name: command`"))?;
        let (name, command) = (name.trim(), command.trim());
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(format!("Procfile.dev line `{line}` should start with a one-word name"));
        }
        if command.is_empty() {
            return Err(format!("Procfile.dev process `{name}` has no command"));
        }
        if entries.iter().any(|e| e.name == name) {
            return Err(format!("Procfile.dev names `{name}` twice"));
        }
        entries.push(Entry { name: name.to_owned(), command: command.to_owned() });
    }
    if entries.is_empty() {
        return Err("Procfile.dev has no processes".into());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_commands_and_comments() {
        let entries = parse("# comment\n\nweb: $APP serve\n  css:  tailwindcss --watch -o a:b \n").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], Entry { name: "web".into(), command: "$APP serve".into() });
        assert!(entries[0].uses_app());
        assert_eq!(entries[1].command, "tailwindcss --watch -o a:b");
        assert!(!entries[1].uses_app());
        assert!(parse("worker: ${APP} work").unwrap()[0].uses_app());
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(parse("").unwrap_err().contains("no processes"));
        assert!(parse("web $APP serve").unwrap_err().contains("name: command"));
        assert!(parse("web:").unwrap_err().contains("no command"));
        assert!(parse("my web: x").unwrap_err().contains("one-word"));
        assert!(parse("web: a\nweb: b").unwrap_err().contains("twice"));
    }

    #[test]
    fn default_runs_the_app() {
        assert!(parse(DEFAULT).unwrap()[0].uses_app());
    }
}
