//! `kiln check`: compile, then check conventions, schema drift and template routes.
//! Every finding has a stable code so agents can look it up and fix it mechanically.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use quote::ToTokens;
use syn::{Attribute, Item, spanned::Spanned};

use crate::{app::App, names::snake};

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Error,
    Warning,
}

struct Finding {
    level: Level,
    code: &'static str,
    message: String,
    file: Option<String>,
    line: Option<usize>,
}

#[derive(Default)]
struct Findings(Vec<Finding>);

impl Findings {
    fn error(&mut self, code: &'static str, message: impl Into<String>, file: Option<&str>, line: Option<usize>) {
        self.push(Level::Error, code, message.into(), file, line);
    }

    fn warn(&mut self, code: &'static str, message: impl Into<String>, file: Option<&str>, line: Option<usize>) {
        self.push(Level::Warning, code, message.into(), file, line);
    }

    fn push(&mut self, level: Level, code: &'static str, message: String, file: Option<&str>, line: Option<usize>) {
        self.0.push(Finding { level, code, message, file: file.map(str::to_owned), line });
    }
}

pub fn run(app: &App, json: bool) -> Result<bool, String> {
    let mut findings = Findings::default();

    eprintln!("kiln check: compiling");
    let errors = compile_errors(app)?;
    let compiled = errors.is_empty();
    if let Some(first) = errors.first() {
        eprintln!("\n{first}");
        if errors.len() > 1 {
            eprintln!("{} more error(s), often caused by the one above:", errors.len() - 1);
            for e in &errors[1..] {
                let mut lines = e.lines();
                let headline = lines.next().unwrap_or_default();
                let location = lines.find(|l| l.trim_start().starts_with("-->")).unwrap_or_default().trim();
                eprintln!("  {headline}  {location}");
            }
            eprintln!();
        }
        findings.error(
            "K0001",
            format!("the app doesn't compile ({} error(s)); fix the first one shown above, then re-run", errors.len()),
            None,
            None,
        );
    }

    eprintln!("kiln check: conventions");
    let sources = rust_files(&app.path("src"));
    let tests = rust_files(&app.path("tests"))
        .into_iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let guarded = guarded_records(app);
    for path in &sources {
        let relative = path.strip_prefix(&app.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let Ok(text) = fs::read_to_string(path) else { continue };
        lint_text(&relative, &text, &mut findings);
        if let Ok(file) = syn::parse_file(&text) {
            lint_items(&relative, &file.items, &tests, &mut findings);
            lint_writes(&relative, &file.items, &guarded, &mut findings);
        }
    }

    if compiled {
        eprintln!("kiln check: schema and templates");
        match app.binary_output(&["db", "status", "--json"])? {
            Some(out) => {
                let status: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
                if status["drift"].as_bool() == Some(true) {
                    findings.error(
                        "K0202",
                        "the record structs changed since the last migration; run `kiln db diff <name>` and review the SQL",
                        Some("db/snapshot.json"),
                        None,
                    );
                }
                for pending in status["pending"].as_array().into_iter().flatten().filter_map(|p| p.as_str()) {
                    findings.warn(
                        "K0203",
                        format!(
                            "migration `{pending}` isn't applied to the development database; run `kiln db migrate`"
                        ),
                        Some(&format!("db/migrations/{pending}.sql")),
                        None,
                    );
                }
            }
            None => findings.error("K0002", "couldn't run `db status` in the app binary", None, None),
        }
        if let Some(out) = app.binary_output(&["manifest"])? {
            let manifest: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
            let routes: BTreeSet<String> = manifest["routes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["action"].as_str().map(str::to_owned))
                .collect();
            lint_templates(app, &routes, &mut findings);
        }
    }

    report(&findings, json);
    Ok(!findings.0.iter().any(|f| f.level == Level::Error))
}

/// Compiler errors from `cargo check --all-targets`, rendered as rustc prints them,
/// de-duplicated (the library compiles once per target) and in order.
fn compile_errors(app: &App) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("cargo")
        .args(["check", "--all-targets", "--message-format=json", "--manifest-path"])
        .arg(app.path("Cargo.toml"))
        .output()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    let mut errors: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let diagnostic = &message["message"];
        let has_spans = diagnostic["spans"].as_array().is_some_and(|s| !s.is_empty());
        if message["reason"] == "compiler-message" && diagnostic["level"] == "error" && has_spans {
            let rendered = diagnostic["rendered"].as_str().unwrap_or_default().trim_end().to_owned();
            if !errors.contains(&rendered) {
                errors.push(rendered);
            }
        }
    }
    if errors.is_empty() && !output.status.success() {
        // Not a compiler error (a bad manifest, a rustc crash): show cargo's own output.
        errors.push(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(errors)
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    files_with(dir, "rs")
}

fn files_with(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_with(&path, extension));
        } else if path.to_string_lossy().ends_with(extension) {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Text-level rules: where raw SQL and environment variables may appear.
fn lint_text(file: &str, text: &str, findings: &mut Findings) {
    for (i, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        if code.contains("sqlx::") && !file.starts_with("src/records/") {
            findings.error(
                "K0501",
                "raw `sqlx` outside src/records/; use the query builder, or put the SQL in the record's file",
                Some(file),
                Some(i + 1),
            );
        }
        if (code.contains("std::env::var") || code.contains("env::var(")) && file != "src/main.rs" {
            findings.error(
                "K0601",
                "environment variables are read in src/main.rs (or kiln::Cli), not in app code; pass the value in",
                Some(file),
                Some(i + 1),
            );
        }
    }
}

fn derives(attrs: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(last) = meta.path.segments.last() {
                out.push(last.ident.to_string());
            }
            Ok(())
        });
    }
    out
}

fn has_attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a Attribute> {
    attrs.iter().find(|a| a.path().segments.last().is_some_and(|s| s.ident == name))
}

/// Structural rules: each kind of item lives in its own directory, one action per file.
fn lint_items(file: &str, items: &[Item], tests: &str, findings: &mut Findings) {
    let mut actions = Vec::new();
    for item in items {
        match item {
            Item::Struct(s) => {
                let line = Some(s.ident.span().start().line);
                for derive in derives(&s.attrs) {
                    match derive.as_str() {
                        "Record" => {
                            let expected = format!("src/records/{}.rs", snake(&s.ident.to_string()));
                            if file != expected {
                                findings.error(
                                    "K0101",
                                    format!("record `{}` belongs in {expected}: its columns are generated as items of that module", s.ident),
                                    Some(file),
                                    line,
                                );
                            }
                        }
                        "Form" if !file.starts_with("src/forms/") => {
                            findings.error(
                                "K0101",
                                format!("form `{}` belongs in src/forms/", s.ident),
                                Some(file),
                                line,
                            );
                        }
                        "View" if !file.starts_with("src/views/") => {
                            findings.error(
                                "K0101",
                                format!("view `{}` belongs in src/views/", s.ident),
                                Some(file),
                                line,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Item::Fn(f) => {
                if let Some(attr) = has_attr(&f.attrs, "action") {
                    actions.push((f.sig.ident.to_string(), attr.span().start().line, attr));
                }
            }
            Item::Impl(i) => {
                if has_attr(&i.attrs, "part").is_some() && !file.starts_with("src/parts/") {
                    findings.error(
                        "K0101",
                        "`#[part]` blocks belong in src/parts/",
                        Some(file),
                        Some(i.span().start().line),
                    );
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    lint_items(file, inner, tests, findings);
                }
            }
            _ => {}
        }
    }

    if actions.is_empty() {
        return;
    }
    if !file.starts_with("src/actions/") {
        findings.error(
            "K0101",
            "`#[action]` functions belong in src/actions/<resource>/<verb>.rs",
            Some(file),
            Some(actions[0].1),
        );
        return;
    }
    if actions.len() > 1 {
        findings.error(
            "K0102",
            format!("{} actions in one file; each action gets its own file so its path helper is `<resource>::<verb>::path`", actions.len()),
            Some(file),
            Some(actions[1].1),
        );
    }
    let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let (name, line, attr) = &actions[0];
    if name != stem {
        findings.warn(
            "K0103",
            format!("action `{name}` lives in `{stem}.rs`; name the function after its file"),
            Some(file),
            Some(*line),
        );
    }
    let method = attr.meta.require_list().ok().map(|l| l.tokens.to_string()).unwrap_or_default();
    let mutating = ["POST", "PUT", "PATCH", "DELETE"].iter().any(|m| method.starts_with(m));
    if let Some(route) = file.strip_prefix("src/actions/").and_then(|r| r.strip_suffix(".rs")) {
        let route = route.replace('/', "::");
        if mutating && !tests.contains(&format!("{route}::path")) {
            findings.warn(
                "K0302",
                format!(
                    "no test calls `{route}::path`; actions that change data need a test (including the forbidden case)"
                ),
                Some(file),
                Some(*line),
            );
        }
    }
}

/// Records that have a policy: every `X` and `NewX` named in `impl Allows<_, X>`.
fn guarded_records(app: &App) -> BTreeSet<String> {
    let mut guarded = BTreeSet::new();
    for path in rust_files(&app.path("src/policies")) {
        let Ok(text) = fs::read_to_string(path) else { continue };
        for chunk in text.split("Allows<").skip(1) {
            let Some(args) = chunk.split('>').next() else { continue };
            let Some(record) = args.split(',').nth(1).map(str::trim) else { continue };
            let record = record.strip_prefix("New").filter(|r| r.starts_with(char::is_uppercase)).unwrap_or(record);
            guarded.insert(record.to_owned());
        }
    }
    guarded
}

/// K0301: writes to a policy-guarded record need the proof that the policy passed.
fn lint_writes(file: &str, items: &[Item], guarded: &BTreeSet<String>, findings: &mut Findings) {
    let exempt = file.starts_with("src/records/") || file == "src/seed.rs" || file == "src/factories.rs";
    if exempt || guarded.is_empty() {
        return;
    }
    for item in items {
        match item {
            Item::Fn(f) => check_write(file, &f.sig, &f.block.to_token_stream().to_string(), guarded, findings),
            Item::Impl(i) => {
                for inner in &i.items {
                    if let syn::ImplItem::Fn(f) = inner {
                        check_write(file, &f.sig, &f.block.to_token_stream().to_string(), guarded, findings);
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    lint_writes(file, inner, guarded, findings);
                }
            }
            _ => {}
        }
    }
}

fn check_write(file: &str, sig: &syn::Signature, body: &str, guarded: &BTreeSet<String>, findings: &mut Findings) {
    let params = sig.inputs.iter().map(|i| i.to_token_stream().to_string()).collect::<Vec<_>>().join(" ");
    for record in guarded {
        let Some(write) = ["insert", "update", "delete"].iter().find(|w| body.contains(&format!("{record} :: {w} (")))
        else {
            continue;
        };
        let proven = params.contains("Authorized")
            && (params.contains(&format!("{record} >")) || params.contains(&format!("New{record} >")));
        if !proven {
            findings.error(
                "K0301",
                format!(
                    "`{}` calls `{record}::{write}` without an `Authorized<_, {record}>` argument; authorize \
                     first (`user.authorize::<Update, _>(record)?`) and pass the proof to an op that does the write",
                    sig.ident
                ),
                Some(file),
                Some(sig.ident.span().start().line),
            );
        }
    }
}

/// Every `path("..")` in a template names a real route; every include/extends exists.
fn lint_templates(app: &App, routes: &BTreeSet<String>, findings: &mut Findings) {
    let root = app.path("templates");
    for path in files_with(&root, ".jinja") {
        let relative = path.strip_prefix(&app.root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let Ok(text) = fs::read_to_string(&path) else { continue };
        for (i, line) in text.lines().enumerate() {
            for (needle, check_route) in [("path(\"", true), ("include \"", false), ("extends \"", false)] {
                let mut rest = line;
                while let Some(start) = rest.find(needle) {
                    let after = &rest[start + needle.len()..];
                    let Some(end) = after.find('"') else { break };
                    let name = &after[..end];
                    if check_route && !routes.contains(name) {
                        findings.error(
                            "K0402",
                            format!(
                                "`path(\"{name}\")` names no route; routes are `<resource>::<verb>` (see `kiln routes`)"
                            ),
                            Some(&relative),
                            Some(i + 1),
                        );
                    }
                    if !check_route && !root.join(name).exists() {
                        findings.error(
                            "K0403",
                            format!("template `{name}` doesn't exist"),
                            Some(&relative),
                            Some(i + 1),
                        );
                    }
                    rest = &after[end..];
                }
            }
        }
    }
}

fn report(findings: &Findings, json: bool) {
    let errors = findings.0.iter().filter(|f| f.level == Level::Error).count();
    let warnings = findings.0.len() - errors;
    if json {
        let list: Vec<_> = findings
            .0
            .iter()
            .map(|f| {
                serde_json::json!({
                    "level": if f.level == Level::Error { "error" } else { "warning" },
                    "code": f.code, "message": f.message, "file": f.file, "line": f.line,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "errors": errors, "warnings": warnings, "findings": list }));
        return;
    }
    for f in &findings.0 {
        let level = if f.level == Level::Error { "error" } else { "warning" };
        println!("{level}[{}]: {}", f.code, f.message);
        if let Some(file) = &f.file {
            match f.line {
                Some(line) => println!("  --> {file}:{line}"),
                None => println!("  --> {file}"),
            }
        }
    }
    let plural = |n: usize, word: &str| if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") };
    println!("\nkiln check: {}, {}", plural(errors, "error"), plural(warnings, "warning"));
}
