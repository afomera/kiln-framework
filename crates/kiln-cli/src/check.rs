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
    code: String,
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
        self.0.push(Finding { level, code: code.to_owned(), message, file: file.map(str::to_owned), line });
    }
}

pub fn run(app: &App, json: bool) -> Result<bool, String> {
    let (findings, _) = collect(app)?;
    report(&findings, json);
    Ok(!findings.0.iter().any(|f| f.level == Level::Error))
}

/// `kiln check` as JSON, for `kiln mcp`: the report plus the compiler errors in full.
pub fn to_json(app: &App) -> Result<serde_json::Value, String> {
    let (findings, compiler_errors) = collect(app)?;
    let mut out = findings_json(&findings);
    out["compiler_errors"] = serde_json::json!(compiler_errors);
    Ok(out)
}

/// Runs every check; progress goes to stderr. The findings, and the compiler errors.
fn collect(app: &App) -> Result<(Findings, Vec<String>), String> {
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
            lint_transactions(&relative, &file, &mut findings);
        }
    }
    lint_javascript(app, &mut findings);

    if compiled {
        eprintln!("kiln check: schema and templates");
        match app.binary_output(&["db", "status", "--json"])? {
            Some(out) => {
                let status: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
                if let Some(stale) = status["stale_snapshot"].as_str() {
                    findings.error("K0204", stale.trim_start_matches("bad request: "), Some("db/snapshot.json"), None);
                } else if status["drift"].as_bool() == Some(true) {
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
        match crate::introspect::report(app) {
            Ok(report) => {
                let routes: BTreeSet<String> = report["routes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["action"].as_str().map(str::to_owned))
                    .collect();
                lint_templates(app, &routes, &mut findings);
                lint_route_conflicts(&report, &mut findings);
                lint_template_variables(app, &report, &mut findings)?;
            }
            Err(e) => findings.error("K0002", format!("couldn't introspect the app: {e}"), None, None),
        }
        app_lints(app, &mut findings)?;
    }
    Ok((findings, errors))
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
        if code.contains("as_system") && file.starts_with("src/actions/") {
            findings.error(
                "K0304",
                "`as_system` in an action skips the viewer's policy; authorize the signed-in record instead (`user.authorize::<Update, _>(record)?`)",
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
                if let Some(attr) = has_attr(&f.attrs, "lint")
                    && !file.starts_with("src/lints/")
                {
                    findings.error(
                        "K0101",
                        "`#[lint]` rules belong in src/lints/",
                        Some(file),
                        Some(attr.span().start().line),
                    );
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

/// K0701: inside `db.transaction(|tx| ..)`, jobs, mail and notifications go on `tx`. On the
/// pool they're stored at once, so a worker can run them before the write commits, or
/// after it rolled back.
fn lint_transactions(file: &str, syntax: &syn::File, findings: &mut Findings) {
    use syn::visit::Visit;

    struct Transactions<'a> {
        file: &'a str,
        findings: &'a mut Findings,
        /// The transaction handles in scope (`tx`).
        handles: Vec<String>,
    }

    impl<'ast> Visit<'ast> for Transactions<'_> {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let closure =
                (call.method == "transaction").then(|| call.args.first()).flatten().and_then(|arg| match arg {
                    syn::Expr::Closure(c) => Some(c),
                    _ => None,
                });
            if let Some(closure) = closure {
                self.visit_expr(&call.receiver);
                let handle = closure.inputs.first().and_then(|p| match p {
                    syn::Pat::Ident(i) => Some(i.ident.to_string()),
                    _ => None,
                });
                if let Some(handle) = handle {
                    self.handles.push(handle);
                    self.visit_expr(&closure.body);
                    self.handles.pop();
                    return;
                }
            }
            let queues = ["enqueue", "enqueue_in", "enqueue_at", "deliver_later", "notify"];
            if let Some(handle) = self.handles.last()
                && queues.iter().any(|m| call.method == m)
                && let Some(db) = call.args.first()
            {
                let mut target = db;
                while let syn::Expr::Reference(r) = target {
                    target = &r.expr;
                }
                let on_handle = matches!(target, syn::Expr::Path(p) if p.path.is_ident(handle.as_str()));
                if !on_handle {
                    let db_text = db.to_token_stream().to_string().replace(' ', "");
                    self.findings.warn(
                        "K0701",
                        format!(
                            "`.{}({db_text})` inside a transaction runs on the pool, so it can happen before the write commits or after it rolls back; pass `&{handle}`",
                            call.method
                        ),
                        Some(self.file),
                        Some(call.method.span().start().line),
                    );
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    Transactions { file, findings, handles: vec![] }.visit_file(syntax);
}

/// K0110: two routes that match the same requests. The router refuses to start with them.
fn lint_route_conflicts(report: &serde_json::Value, findings: &mut Findings) {
    let mut seen: std::collections::BTreeMap<(String, String), (String, String, u64)> = Default::default();
    for r in report["routes"].as_array().into_iter().flatten() {
        let method = r["method"].as_str().unwrap_or_default().to_owned();
        let path = r["path"].as_str().unwrap_or_default();
        // `{slug}` and `{id}` in the same place match the same URLs.
        let shape: String =
            path.split('/').map(|seg| if seg.starts_with('{') { "{}" } else { seg }).collect::<Vec<_>>().join("/");
        let action = r["action"].as_str().unwrap_or_default().to_owned();
        let file = r["file"].as_str().unwrap_or_default().to_owned();
        let line = r["line"].as_u64().unwrap_or(0);
        if let Some((other, other_file, other_line)) = seen.get(&(method.clone(), shape.clone())) {
            findings.error(
                "K0110",
                format!(
                    "`{method} {path}` ({action}) matches the same requests as {other} ({other_file}:{other_line}); \
                     the server won't start. Change one path, or merge the actions"
                ),
                Some(&file),
                Some(line as usize),
            );
        } else {
            seen.insert((method, shape), (action, file, line));
        }
    }
}

/// K0401, K0412: templates read only what their view (or mail) provides.
fn lint_template_variables(app: &App, report: &serde_json::Value, findings: &mut Findings) -> Result<(), String> {
    use crate::template_check::{Records, check, shape_of};
    let Some(out) = app.binary_output(&["templates", "--variables"])? else {
        findings.error("K0002", "couldn't list the templates' variables (`templates --variables`)", None, None);
        return Ok(());
    };
    let analysis: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
    let globals: BTreeSet<String> =
        analysis["globals"].as_array().into_iter().flatten().filter_map(|g| g.as_str().map(str::to_owned)).collect();
    let records = Records::from_introspection(report);
    let owners = report["views"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| (v, &["html"][..]))
        .chain(report["mail"].as_array().into_iter().flatten().map(|m| (m, &["html", "txt"][..])));
    for (owner, extensions) in owners {
        let Some(template) = owner["template"].as_str() else { continue };
        let name = owner["name"].as_str().unwrap_or_default();
        let fields = owner["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f["name"].as_str()?.to_owned(), shape_of(f["type"].as_str()?, &records))))
            .collect();
        for extension in extensions {
            let file = format!("{template}.{extension}.jinja");
            let Some(used) = analysis["templates"][&file].as_array() else { continue };
            let used: Vec<String> = used.iter().filter_map(|u| u.as_str().map(str::to_owned)).collect();
            let relative = format!("templates/{file}");
            let source = fs::read_to_string(app.path(&relative)).unwrap_or_default();
            for problem in check(name, &source, &used, &fields, &globals, &records) {
                let line = find_line(&source, &problem.needle);
                findings.error(problem.code, problem.message, Some(&relative), line);
            }
        }
    }
    Ok(())
}

/// The first line where `needle` appears as a whole name (not inside a longer one).
fn find_line(source: &str, needle: &str) -> Option<usize> {
    source
        .lines()
        .position(|line| {
            line.match_indices(needle).any(|(i, _)| {
                let before = line[..i].chars().last();
                let after = line[i + needle.len()..].chars().next();
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                !word(before) && before != Some('.') && !word(after)
            })
        })
        .map(|i| i + 1)
}

/// The app's own rules (`#[lint]` in src/lints/), run by its binary.
fn app_lints(app: &App, findings: &mut Findings) -> Result<(), String> {
    if !app.path("src/lints").is_dir() {
        return Ok(());
    }
    eprintln!("kiln check: the app's rules");
    let Some(out) = app.binary_output(&["lint"])? else {
        findings.error("K0003", "couldn't run the app's own rules (`lint` in the app binary)", None, None);
        return Ok(());
    };
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
    for f in report["findings"].as_array().into_iter().flatten() {
        let level = if f["level"] == "warning" { Level::Warning } else { Level::Error };
        findings.0.push(Finding {
            level,
            code: f["code"].as_str().unwrap_or("A0000").to_owned(),
            message: f["message"].as_str().unwrap_or_default().to_owned(),
            file: f["file"].as_str().map(str::to_owned),
            line: f["line"].as_u64().map(|l| l as usize),
        });
    }
    Ok(())
}

/// K0404: every `data-controller` in a template names a Stimulus controller the app has.
/// K0405: every pin in config/importmap.toml loads.
fn lint_javascript(app: &App, findings: &mut Findings) {
    let mut known: BTreeSet<String> =
        crate::importmap::controllers(&app.root).into_iter().map(|(identifier, _)| identifier).collect();
    // Controllers registered by hand: `application.register("sortable", Sortable)`.
    for path in files_with(&app.path("assets"), ".js") {
        if path.starts_with(app.path("assets/vendor")) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        known.extend(registered_controllers(&text));
    }
    for path in files_with(&app.path("templates"), ".jinja") {
        let relative = path.strip_prefix(&app.root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let Ok(text) = fs::read_to_string(&path) else { continue };
        for (i, line) in text.lines().enumerate() {
            for identifier in data_controllers(line) {
                if !known.contains(identifier) {
                    let file =
                        format!("assets/controllers/{}_controller.js", identifier.replace("--", "/").replace('-', "_"));
                    findings.error(
                        "K0404",
                        format!(
                            "`data-controller=\"{identifier}\"` has no controller; add {file} (`kiln g controller {}`)",
                            identifier.replace("--", "/").replace('-', "_")
                        ),
                        Some(&relative),
                        Some(i + 1),
                    );
                }
            }
        }
    }
    let pins_file = crate::importmap::PINS;
    let Ok(text) = fs::read_to_string(app.path(pins_file)) else { return };
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => {
            findings.error("K0405", format!("{pins_file} isn't valid TOML: {}", e.message()), Some(pins_file), None);
            return;
        }
    };
    for (name, to) in table {
        let line = text.lines().position(|l| l.contains(&name)).map(|i| i + 1);
        match to.as_str() {
            None => findings.error(
                "K0405",
                format!("`{name}` should be a string: a file under assets/ or a URL"),
                Some(pins_file),
                line,
            ),
            Some(to)
                if !to.starts_with("https://")
                    && !to.starts_with("http://")
                    && !app.path("assets").join(to).is_file() =>
            {
                findings.error(
                    "K0405",
                    format!("`{name}` is pinned to `{to}`, which isn't in assets/"),
                    Some(pins_file),
                    line,
                )
            }
            Some(_) => {}
        }
    }
}

/// The names in a line's `data-controller="a b"` attributes. Values with template
/// expressions aren't checked.
fn data_controllers(line: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = line;
    while let Some(start) = rest.find("data-controller=") {
        let after = &rest[start + "data-controller=".len()..];
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            rest = after;
            continue;
        };
        let Some(end) = after[1..].find(quote) else { break };
        let value = &after[1..1 + end];
        if !value.contains('{') {
            out.extend(value.split_whitespace());
        }
        rest = &after[1 + end..];
    }
    out
}

/// `register("name", ..)` calls in an app's own JavaScript.
fn registered_controllers(source: &str) -> Vec<String> {
    source
        .split(".register(")
        .skip(1)
        .filter_map(|call| {
            let call = call.trim_start();
            let quote = call.chars().next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
            let end = call[1..].find(quote)?;
            Some(call[1..1 + end].to_owned())
        })
        .collect()
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

fn findings_json(findings: &Findings) -> serde_json::Value {
    let errors = findings.0.iter().filter(|f| f.level == Level::Error).count();
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
    serde_json::json!({ "errors": errors, "warnings": findings.0.len() - errors, "findings": list })
}

fn report(findings: &Findings, json: bool) {
    let errors = findings.0.iter().filter(|f| f.level == Level::Error).count();
    let warnings = findings.0.len() - errors;
    if json {
        println!("{}", findings_json(findings));
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
    if let Some(f) = findings.0.first() {
        println!("`kiln explain {}` (or any code above) says why a rule exists and how to fix it", f.code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(findings: &Findings) -> Vec<(String, Option<usize>)> {
        findings.0.iter().map(|f| (f.code.clone(), f.line)).collect()
    }

    #[test]
    fn jobs_inside_a_transaction_go_on_it() {
        let source = r#"
async fn op(ctx: &Ctx) -> Result {
    ctx.db.transaction(|tx| async move {
        Post::insert(new).run(&tx).await?;
        Welcome { id }.enqueue(&tx).await?;
        Welcome { id }.enqueue(&ctx.db).await?;
        mail.deliver_later(&ctx.db).await?;
        Ok(())
    }).await?;
    Welcome { id }.enqueue(&ctx.db).await?;
    Ok(())
}
"#;
        let mut findings = Findings::default();
        lint_transactions("src/ops/op.rs", &syn::parse_file(source).unwrap(), &mut findings);
        assert_eq!(codes(&findings), [("K0701".into(), Some(6)), ("K0701".into(), Some(7))]);
    }

    #[test]
    fn reads_stimulus_controller_names() {
        assert_eq!(data_controllers(r#"<div data-controller="tabs admin--users" id="x">"#), ["tabs", "admin--users"]);
        assert_eq!(data_controllers("<a data-controller='copy'>"), ["copy"]);
        assert!(data_controllers(r#"<div data-controller="{{ name }}">"#).is_empty());
        assert_eq!(
            registered_controllers("application.register(\"sortable\", S)\nStimulus.register('x', X)"),
            ["sortable", "x"]
        );
    }

    #[test]
    fn templates_name_controllers_that_exist_and_pins_load() {
        let dir = std::env::temp_dir().join(format!("kiln-check-js-{}", std::process::id()));
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("assets/controllers/admin/users_controller.js", "");
        write(
            "assets/application.js",
            "import { application } from \"kiln/stimulus\"\napplication.register(\"sortable\", Sortable)\n",
        );
        write(
            "templates/posts/index.html.jinja",
            "<div data-controller=\"admin--users sortable\">\n<div data-controller=\"hello-world\">\n",
        );
        write("config/importmap.toml", "ok = \"https://esm.sh/ok@1\"\ngone = \"vendor/gone.js\"\n");
        let app = App { root: dir.clone(), package: "demo".into() };
        let mut findings = Findings::default();
        lint_javascript(&app, &mut findings);
        assert_eq!(codes(&findings), [("K0404".into(), Some(2)), ("K0405".into(), Some(2))]);
        assert!(
            findings.0[0].message.contains("assets/controllers/hello_world_controller.js"),
            "{}",
            findings.0[0].message
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn routes_matching_the_same_requests_conflict() {
        let report = serde_json::json!({ "routes": [
            { "method": "GET", "path": "/posts/{slug}", "action": "posts::show", "file": "src/actions/posts/show.rs", "line": 5 },
            { "method": "GET", "path": "/posts/new", "action": "posts::new", "file": "src/actions/posts/new.rs", "line": 5 },
            { "method": "PATCH", "path": "/posts/{slug}", "action": "posts::update", "file": "src/actions/posts/update.rs", "line": 5 },
            { "method": "GET", "path": "/posts/{id}", "action": "posts::preview", "file": "src/actions/posts/preview.rs", "line": 7 },
        ]});
        let mut findings = Findings::default();
        lint_route_conflicts(&report, &mut findings);
        assert_eq!(codes(&findings), [("K0110".into(), Some(7))]);
        assert!(findings.0[0].message.contains("posts::show"));
    }
}
