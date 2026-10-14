//! `kiln mcp`: the app as an MCP server, for agents. Speaks JSON-RPC over stdin and stdout,
//! so an agent starts it itself (`claude mcp add kiln -- kiln mcp`, or the `.mcp.json` new
//! apps get) and nothing has to be running first.
//!
//! The tools look at the app the way a person debugging it would: the whole app
//! (`introspect`), a request made as any user with the SQL it ran and the jobs and mail it
//! caused (`request`), read-only SQL (`query`), the outbox (`mail`), the queues (`jobs`),
//! and `kiln check`, `kiln explain` and the tests. Everything that needs the app runs its
//! development binary, rebuilt first if the source changed, against the development
//! database.
//!
//! Read-only by default: requests other than GET and HEAD need `kiln mcp --write`, since
//! they change the development database.

use std::{
    io::{BufRead, Write},
    process::Command,
};

use serde_json::{Value, json};

use crate::app::App;

const PROTOCOL: &str = "2025-06-18";

pub fn run(app: &App, write: bool) -> Result<bool, String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = handle(app, write, &line) else { continue };
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// One JSON-RPC message in; the reply, if it wants one.
fn handle(app: &App, write: bool, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Some(error(Value::Null, -32700, &format!("not JSON: {e}"))),
    };
    // Notifications (no id) get no reply.
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or(PROTOCOL);
            let version = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&asked) { asked } else { PROTOCOL };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "kiln", "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions(app, write),
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools(write) }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(app, write, name, &args) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
            }
        }
        method => return Some(error(id, -32601, &format!("no method `{method}`"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn instructions(app: &App, write: bool) -> String {
    format!(
        "Tools for the Kiln app `{}`. Start with `introspect` to see the app; check work with `request` \
         (a page as any user, with its SQL, jobs and mail), `check` and `test`; `explain` a code from \
         either. Requests other than GET are {}.",
        app.package,
        if write {
            "allowed (this server was started with --write)"
        } else {
            "refused: start `kiln mcp --write` to allow them"
        }
    )
}

fn tools(write: bool) -> Vec<Value> {
    let object = |properties: Value, required: &[&str]| json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false });
    vec![
        json!({
            "name": "introspect",
            "description": "The whole app: every route (method, path, action, file:line, pipeline, params, signed-in record, form, argument and return types), record (columns, relations), view (template, fields), form (fields, validations), policy, op signature, part, job, schedule, mail, notification, config, template and lint rule. Call it before changing an unfamiliar app.",
            "inputSchema": object(json!({}), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "request",
            "description": format!(
                "Send a request through the app in-process, against the development database, optionally signed in. Returns status, redirects, flash, headers, the body (HTML as readable text by default), every SQL statement it ran, and the jobs and mail it caused. Forms get a valid CSRF token. {}",
                if write { "Any method." } else { "GET and HEAD only (the server wasn't started with --write)." }
            ),
            "inputSchema": object(json!({
                "method": { "type": "string", "description": "GET (default), POST, PATCH, PUT, DELETE, HEAD" },
                "path": { "type": "string", "description": "e.g. /posts/hello" },
                "as": { "type": "string", "description": "Sign in first: `User:3`, `3` (a User id) or an email address" },
                "form": { "type": "object", "description": "Form fields, as a browser submits them: {\"title\": \"Hi\"}", "additionalProperties": { "type": "string" } },
                "json": { "description": "A JSON body instead of a form" },
                "accept": { "type": "string", "enum": ["html", "json", "turbo"], "description": "What the client accepts (default html)" },
                "follow": { "type": "boolean", "description": "Follow redirects (up to 5), reporting each" },
                "body": { "type": "string", "enum": ["text", "html", "none"], "description": "How to return an HTML body: readable text (default), the raw HTML, or not at all" },
            }), &["path"]),
            "annotations": { "readOnlyHint": !write },
        }),
        json!({
            "name": "query",
            "description": "Run read-only SQL against the development database (the connection refuses writes). Kiln's own tables: kiln_jobs, kiln_mail, kiln_sessions, kiln_blobs, kiln_cache, kiln_streams.",
            "inputSchema": object(json!({
                "sql": { "type": "string" },
                "limit": { "type": "integer", "description": "Rows to return (default 100)" },
                "database": { "type": "string", "enum": ["main", "queue", "cache", "streams"], "description": "For apps with split databases (default main)" },
            }), &["sql"]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "mail",
            "description": "The newest mail in the development outbox: to, from, subject, text and HTML.",
            "inputSchema": object(json!({ "limit": { "type": "integer", "description": "default 10" } }), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "jobs",
            "description": "Background jobs: counts by status, queues (waiting, running, failed, paused) and failed jobs with their errors.",
            "inputSchema": object(json!({}), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "check",
            "description": "Run `kiln check`: compile, conventions, schema drift, template links and the app's own rules. Returns findings with codes, files and lines, and compiler errors in full. Done means 0 errors.",
            "inputSchema": object(json!({}), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "explain",
            "description": "Explain a code from `check` or an error page (K0301, K1203, or the app's own A0001): why the rule exists and how to fix it. Without a code, lists every code.",
            "inputSchema": object(json!({ "code": { "type": "string" } }), &[]),
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": "test",
            "description": "Run the app's tests (`cargo test`), optionally only those whose names contain `filter`. Returns pass/fail and the end of the output.",
            "inputSchema": object(json!({ "filter": { "type": "string" } }), &[]),
            "annotations": { "readOnlyHint": true },
        }),
    ]
}

fn call(app: &App, write: bool, name: &str, args: &Value) -> Result<String, String> {
    let s = |key: &str| args[key].as_str().map(str::to_owned);
    match name {
        "introspect" => pretty(&crate::introspect::report(app)?),
        "request" => {
            let method = s("method").unwrap_or_else(|| "GET".into()).to_ascii_uppercase();
            let path = s("path").ok_or("`path` is required")?;
            if !write && !matches!(method.as_str(), "GET" | "HEAD") {
                return Err(format!(
                    "{method} changes the development database, and this server is read-only; restart it as `kiln mcp --write` to allow it"
                ));
            }
            let mut argv = vec!["request".to_owned(), method, path];
            if let Some(who) = s("as") {
                argv.extend(["--as".into(), who]);
            }
            for (key, value) in args["form"].as_object().into_iter().flatten() {
                let value = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
                argv.extend(["--form".into(), format!("{key}={value}")]);
            }
            if let Some(body) = args.get("json").filter(|v| !v.is_null()) {
                argv.extend(["--json".into(), body.to_string()]);
            }
            if let Some(accept) = s("accept") {
                argv.extend(["--accept".into(), accept]);
            }
            if args["follow"].as_bool() == Some(true) {
                argv.push("--follow".into());
            }
            let mut response = binary_json(app, &argv)?;
            let html = response["content_type"].as_str().is_some_and(|t| t.starts_with("text/html"));
            match s("body").as_deref() {
                Some("none") => response["body"] = Value::Null,
                Some("html") => {}
                // A development error page: its Markdown report says it better.
                _ if response["error_report"].is_string() => {
                    response["body"] = Value::Null;
                }
                _ if html => response["body"] = json!(html_to_text(response["body"].as_str().unwrap_or_default())),
                _ => {}
            }
            pretty(&response)
        }
        "query" => {
            let mut argv = vec!["db".to_owned(), "query".into(), s("sql").ok_or("`sql` is required")?];
            if let Some(limit) = args["limit"].as_u64() {
                argv.extend(["--limit".into(), limit.to_string()]);
            }
            if let Some(database) = s("database") {
                argv.extend(["--database".into(), database]);
            }
            pretty(&binary_json(app, &argv)?)
        }
        "mail" => {
            let limit = args["limit"].as_u64().unwrap_or(10).to_string();
            let mut out = binary_json(app, &["mail".into(), "--limit".into(), limit])?;
            // The text part says what the HTML does, at a fraction of the length.
            for m in out["mail"].as_array_mut().into_iter().flatten() {
                if !m["text"].is_null() {
                    m["html"] = json!("(omitted; the text part is above)");
                }
            }
            pretty(&out)
        }
        "jobs" => pretty(&binary_json(app, &["jobs".into(), "--json".into()])?),
        "check" => pretty(&crate::check::to_json(app)?),
        "explain" => crate::explain::text(Some(app), s("code").as_deref()),
        "test" => {
            let mut command = Command::new("cargo");
            command.args(["test", "--quiet", "--manifest-path"]).arg(app.path("Cargo.toml"));
            if let Some(filter) = s("filter") {
                command.arg(filter);
            }
            let output = command.output().map_err(|e| format!("couldn't run cargo: {e}"))?;
            let text =
                format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            // A filter leaves every other test binary reporting nothing; drop those.
            let lines: Vec<&str> = text
                .lines()
                .filter(|l| {
                    !l.trim().is_empty() && *l != "running 0 tests" && !l.starts_with("test result: ok. 0 passed")
                })
                .collect();
            let tail = lines[lines.len().saturating_sub(150)..].join("\n");
            let ran: u64 = text
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("test result: ")?.split(". ").nth(1)?.split(' ').next()?.parse::<u64>().ok()
                })
                .sum();
            if output.status.success() && ran == 0 {
                return Ok(match s("filter") {
                    Some(filter) => format!("no test's name contains `{filter}` (test names, not file names)"),
                    None => "the app has no tests".into(),
                });
            }
            Ok(format!(
                "{}\n\n{tail}",
                if output.status.success() { format!("PASSED ({ran} tests)") } else { "FAILED".into() }
            ))
        }
        other => Err(format!("no tool `{other}`")),
    }
}

fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| e.to_string())
}

/// Runs the app binary with `args` and parses its JSON; on failure, the end of what it said.
fn binary_json(app: &App, args: &[String]) -> Result<Value, String> {
    let output = Command::new("cargo")
        .args(["run", "--quiet", "--manifest-path"])
        .arg(app.path("Cargo.toml"))
        .arg("--")
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success()
        && let Ok(value) = serde_json::from_str(stdout.trim())
    {
        return Ok(value);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().chain(stdout.lines()).collect();
    let tail = lines[lines.len().saturating_sub(40)..].join("\n");
    if tail.contains("could not compile") {
        return Err(format!("the app doesn't compile; the `check` tool shows the errors in full:\n{tail}"));
    }
    Err(tail)
}

/// An HTML page as text an agent can read cheaply: headings, paragraphs, list items, links
/// as `[text](href)`, forms and their fields, buttons; scripts, styles and SVG dropped.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut link: Option<String> = None;
    let mut in_button = false;
    while let Some(start) = rest.find('<') {
        let text = decode(&rest[..start]);
        // A button's label stays on one line.
        out.push_str(&if in_button { text.split_whitespace().collect::<Vec<_>>().join(" ") } else { text });
        let Some(end) = rest[start..].find('>') else { break };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let name = tag.trim_start_matches('/').split([' ', '\n', '\t', '/']).next().unwrap_or("").to_ascii_lowercase();
        let closing = tag.starts_with('/');
        if !closing && matches!(name.as_str(), "script" | "style" | "svg" | "head" | "template") {
            let close = format!("</{name}");
            rest = rest.find(&close).map_or("", |i| &rest[i..]);
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
            continue;
        }
        let attr = |key: &str| attribute(tag, key);
        match (name.as_str(), closing) {
            ("br", _) | ("tr", false) => out.push('\n'),
            (
                "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "ul" | "ol" | "table"
                | "figure",
                _,
            ) => out.push('\n'),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = name[1..].parse().unwrap_or(1);
                out.push_str(&format!("\n\n{} ", "#".repeat(level)));
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => out.push('\n'),
            ("li", false) => out.push_str("\n- "),
            ("td" | "th", false) => out.push_str(" | "),
            ("a", false) => {
                link = attr("href");
                out.push('[');
            }
            ("a", true) => {
                out.push_str(&format!("]({})", link.take().unwrap_or_default()));
            }
            ("img", _) => out.push_str(&format!("[image: {}]", attr("alt").unwrap_or_default())),
            ("form", false) => out.push_str(&format!(
                "\n[form {} {}]\n",
                attr("method").unwrap_or_else(|| "get".into()).to_uppercase(),
                attr("action").unwrap_or_default()
            )),
            ("form", true) => out.push_str("\n[/form]\n"),
            ("input" | "textarea" | "select", false) => {
                let kind = attr("type").unwrap_or_else(|| name.clone());
                if kind != "hidden" || attr("name").as_deref() == Some("_method") {
                    let mut field = format!("[{kind} name={}", attr("name").unwrap_or_default());
                    if let Some(value) = attr("value").filter(|v| !v.is_empty()) {
                        field.push_str(&format!(" value={value:?}"));
                    }
                    out.push_str(&field);
                    out.push(']');
                }
            }
            ("button", false) => {
                in_button = true;
                out.push_str("[button: ");
                if let Some(label) = attr("aria-label").or_else(|| attr("title")) {
                    out.push_str(&label);
                }
            }
            ("button", true) => {
                in_button = false;
                out.push(']');
            }
            _ => {}
        }
    }
    out.push_str(&decode(rest));
    // Collapse runs of spaces and blank lines.
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(&line);
        text.push('\n');
    }
    text.trim().to_owned()
}

fn attribute(tag: &str, key: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(key) {
        let before = rest[..i].chars().last();
        let after = &rest[i + key.len()..];
        if before.is_some_and(char::is_whitespace) && after.starts_with('=') {
            let value = &after[1..];
            let (quote, value) = match value.chars().next() {
                Some(q @ ('"' | '\'')) => (q, &value[1..]),
                _ => (' ', value),
            };
            let end = value.find(quote).unwrap_or(value.len());
            return Some(decode(&value[..end]));
        }
        rest = after;
    }
    None
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_read_as_text() {
        let html = r#"<html><head><title>T</title><script>x()</script></head><body>
            <h1>Posts</h1><ul><li><a href="/posts/a">A &amp; B</a></li></ul>
            <form method="post" action="/session"><input type="hidden" name="_csrf" value="t">
            <input type="hidden" name="_method" value="DELETE"><input name="email" type="email">
            <button>Sign out</button></form><button aria-label="Dark theme">
              <svg><path/></svg>
            </button><svg><path d="M0"/></svg><p>Done</p></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("# Posts"), "{text}");
        assert!(text.contains("- [A & B](/posts/a)"), "{text}");
        assert!(text.contains("[form POST /session]"), "{text}");
        assert!(text.contains("[hidden name=_method value=\"DELETE\"]"), "{text}");
        assert!(text.contains("[email name=email]"), "{text}");
        assert!(text.contains("[button: Sign out]"), "{text}");
        assert!(text.contains("[button: Dark theme]"), "{text}");
        assert!(!text.contains("_csrf") && !text.contains("x()") && !text.contains("M0"), "{text}");
    }

    #[test]
    fn speaks_json_rpc() {
        let app = App { root: std::env::temp_dir(), package: "demo".into() };
        let init = handle(
            &app,
            false,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "kiln");
        assert!(handle(&app, false, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let list = handle(&app, false, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
        assert!(names.contains(&json!("request")) && names.contains(&json!("introspect")));
        let refused = handle(
            &app,
            false,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"request","arguments":{"method":"POST","path":"/posts"}}}"#,
        )
        .unwrap();
        assert_eq!(refused["result"]["isError"], true);
        assert!(refused["result"]["content"][0]["text"].as_str().unwrap().contains("--write"));
        let unknown = handle(&app, false, r#"{"jsonrpc":"2.0","id":4,"method":"nope"}"#).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }
}
