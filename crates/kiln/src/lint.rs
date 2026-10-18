//! The app's own `kiln check` rules: a team's conventions, enforced like Kiln's.
//!
//! A rule is a function in `src/lints/` marked `#[lint]`. It gets one file at a time (Rust
//! source parsed with `syn`, templates as text) and reports what it finds:
//!
//! ```ignore
//! use kiln::lint::*;
//!
//! /// Actions return errors with `?`; a panic in a request is a 500 with no explanation.
//! #[lint(code = "A0001")]
//! fn no_unwrap_in_actions(file: &File, report: &mut Report) {
//!     if !file.is_in("src/actions") {
//!         return;
//!     }
//!     for (line, code) in file.code_lines() {
//!         if code.contains(".unwrap()") {
//!             report.at(line, "`.unwrap()` in an action; return the error with `?`");
//!         }
//!     }
//! }
//! ```
//!
//! `kiln check` runs every rule through the app binary (`<app> lint`) after its own, and the
//! findings land in the same report. Codes are a capital letter other than `K` (Kiln's own)
//! and four digits; the doc comment is what `kiln explain <code>` prints. Test a rule by
//! calling [`check`] with a path and some source.

use std::path::Path;

pub use kiln_macros::lint;
pub use syn::{self, spanned::Spanned, visit::Visit};

/// How a finding counts: an error fails `kiln check`; a warning is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
        }
    }
}

/// One file a rule looks at: Rust under `src/` or `tests/`, or a template.
pub struct File {
    /// Relative to the app's root, with `/`: `src/actions/posts/show.rs`.
    pub path: String,
    pub text: String,
    syntax: Option<syn::File>,
}

impl File {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        let path = path.into();
        let text = text.into();
        let syntax = path.ends_with(".rs").then(|| syn::parse_file(&text).ok()).flatten();
        Self { path, text, syntax }
    }

    pub fn is_rust(&self) -> bool {
        self.path.ends_with(".rs")
    }

    pub fn is_template(&self) -> bool {
        self.path.starts_with("templates/")
    }

    /// Whether the file is under `dir` (`"src/actions"`).
    pub fn is_in(&self, dir: &str) -> bool {
        self.path.strip_prefix(dir.trim_end_matches('/')).is_some_and(|rest| rest.starts_with('/'))
    }

    /// The parsed Rust file; `None` for templates and for source that doesn't parse (the
    /// compiler reports that).
    pub fn syntax(&self) -> Option<&syn::File> {
        self.syntax.as_ref()
    }

    /// `(line number, line)`, numbered from 1.
    pub fn lines(&self) -> impl Iterator<Item = (usize, &str)> {
        self.text.lines().enumerate().map(|(i, line)| (i + 1, line))
    }

    /// Like [`File::lines`] without `//` comments, so a rule doesn't flag its own advice.
    pub fn code_lines(&self) -> impl Iterator<Item = (usize, &str)> {
        self.lines().map(|(n, line)| (n, line.split("//").next().unwrap_or_default()))
    }
}

/// What a rule found in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: Option<usize>,
    pub message: String,
}

/// Where a rule reports.
#[derive(Default, Debug)]
pub struct Report {
    findings: Vec<Finding>,
}

impl Report {
    /// A finding on a line (numbered from 1).
    pub fn at(&mut self, line: usize, message: impl Into<String>) {
        self.findings.push(Finding { line: Some(line), message: message.into() });
    }

    /// A finding on the line where a piece of syntax starts.
    pub fn on(&mut self, node: &impl Spanned, message: impl Into<String>) {
        self.at(node.span().start().line, message);
    }

    /// A finding about the whole file.
    pub fn file(&mut self, message: impl Into<String>) {
        self.findings.push(Finding { line: None, message: message.into() });
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
}

/// A registered rule; `#[lint]` writes these.
pub struct LintDef {
    pub code: &'static str,
    pub level: Level,
    pub name: &'static str,
    /// The rule's doc comment: what it enforces and why.
    pub doc: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub check: fn(&File, &mut Report),
}

inventory::collect!(LintDef);

/// Every rule the app registered, by code.
pub fn rules() -> Vec<&'static LintDef> {
    let mut rules: Vec<_> = inventory::iter::<LintDef>.into_iter().collect();
    rules.sort_by_key(|r| r.code);
    rules
}

/// Runs `rule` on one file: for tests.
///
/// ```ignore
/// let found = kiln::lint::check(no_unwrap_in_actions, "src/actions/posts/show.rs", "x.unwrap();");
/// assert_eq!(found.len(), 1);
/// ```
pub fn check(rule: fn(&File, &mut Report), path: &str, text: &str) -> Vec<Finding> {
    let mut report = Report::default();
    rule(&File::new(path, text), &mut report);
    report.findings
}

/// Runs every rule over the app at `root`: Rust in `src/` and `tests/`, and `templates/`.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) fn run(root: &Path) -> serde_json::Value {
    let rules = rules();
    let mut findings = vec![];
    for pair in rules.windows(2).filter(|p| p[0].code == p[1].code) {
        findings.push(serde_json::json!({
            "code": pair[1].code, "level": "error", "file": app_relative(pair[1].file), "line": pair[1].line,
            "message": format!("`{}` and `{}` both use {}; give each rule its own code", pair[0].name, pair[1].name, pair[1].code),
        }));
    }
    let mut files = vec![];
    for dir in ["src", "tests", "templates"] {
        files.extend(walk(&root.join(dir)));
    }
    files.sort();
    // A rule that panics is a bug in the rule, not a finding about the app: report it once,
    // with the panic's message, and stop running that rule.
    let mut broken = vec![false; rules.len()];
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if !(relative.ends_with(".rs") || relative.starts_with("templates/")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let file = File::new(relative, text);
        for (i, rule) in rules.iter().enumerate() {
            if broken[i] {
                continue;
            }
            let mut report = Report::default();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (rule.check)(&file, &mut report)));
            if let Err(panic) = result {
                broken[i] = true;
                let reason = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("a panic");
                findings.push(serde_json::json!({
                    "code": rule.code, "level": "error", "file": app_relative(rule.file), "line": rule.line,
                    "message": format!("rule `{}` panicked on {} ({reason}); fix the rule", rule.name, file.path),
                }));
                continue;
            }
            for f in report.findings {
                findings.push(serde_json::json!({
                    "code": rule.code, "level": rule.level.as_str(), "file": file.path, "line": f.line,
                    "message": f.message,
                }));
            }
        }
    }
    std::panic::set_hook(quiet);
    serde_json::json!({ "rules": rules.len(), "findings": findings })
}

/// The rules themselves, for `kiln explain` and `kiln introspect`.
pub(crate) fn list() -> serde_json::Value {
    rules()
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code, "level": r.level.as_str(), "name": r.name, "doc": r.doc,
                "file": app_relative(r.file), "line": r.line,
            })
        })
        .collect()
}

/// `file!()` is relative to the workspace when the app is in one (`examples/blog/src/..`).
pub(crate) fn app_relative(file: &str) -> &str {
    match file.find("src/") {
        Some(i) if i == 0 || file[..i].ends_with('/') => &file[i..],
        _ => file,
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_unwrap(file: &File, report: &mut Report) {
        if !file.is_in("src/actions") {
            return;
        }
        for (line, code) in file.code_lines() {
            if code.contains(".unwrap()") {
                report.at(line, "no unwrap");
            }
        }
    }

    fn pub_fns(file: &File, report: &mut Report) {
        for item in &file.syntax().expect("parses").items {
            if let syn::Item::Fn(f) = item {
                report.on(&f.sig.ident, format!("fn {}", f.sig.ident));
            }
        }
    }

    #[test]
    fn rules_see_lines_and_syntax() {
        let found = check(
            no_unwrap,
            "src/actions/posts/show.rs",
            "let a = 1;\nlet b = x.unwrap(); // .unwrap() ok\n// y.unwrap()\n",
        );
        assert_eq!(found, vec![Finding { line: Some(2), message: "no unwrap".into() }]);
        assert!(check(no_unwrap, "src/ops/x.rs", "x.unwrap();").is_empty());
        assert!(check(no_unwrap, "src/actionsx/a.rs", "x.unwrap();").is_empty());

        let found = check(pub_fns, "src/ops/a.rs", "use a::b;\n\npub fn one() {}\nfn two() {}\n");
        assert_eq!(found.iter().map(|f| f.line).collect::<Vec<_>>(), [Some(3), Some(4)]);
    }

    #[test]
    fn templates_have_no_syntax() {
        let file = File::new("templates/posts/show.html.jinja", "{{ post.title }}");
        assert!(file.is_template() && !file.is_rust() && file.syntax().is_none());
    }
}
