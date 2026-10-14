//! Error pages for people and agents.
//!
//! In debug builds an error renders as a full report: status, stable code, the request and
//! action, a source excerpt, and concrete fixes. Browsers get HTML with a "Copy as Markdown"
//! button; clients that don't ask for HTML (curl, agents' fetch tools) get the same report as
//! Markdown. Release builds show only the status, with no internals.

use std::fmt::Write as _;

use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};

use crate::routing;

#[derive(Debug, Clone)]
pub struct ErrorReport {
    pub status: StatusCode,
    pub code: &'static str,
    pub title: &'static str,
    pub message: String,
    pub source: Option<SourceExcerpt>,
    pub hints: Vec<String>,
    /// Shown as a table of the app's routes (for 404/405 on unknown paths).
    pub show_routes: bool,
    /// Validation messages by field, for invalid input.
    pub fields: Option<crate::form::Errors>,
    /// For sign-in errors: the table of the record type that must be signed in.
    pub sign_in_for: Option<&'static str>,
    /// Development only: fixes the page can run, shown as buttons ("Run migrations").
    pub actions: Vec<ResolveAction>,
}

/// A fix an error page offers to run, posted to a `/kiln/resolve/..` control.
#[derive(Debug, Clone)]
pub struct ResolveAction {
    pub label: String,
    /// `/kiln/resolve/migrate`
    pub path: String,
    /// The `/kiln` control token.
    pub token: String,
    /// Where to go once it ran: the page that failed.
    pub back: String,
}

impl ErrorReport {
    pub fn new(status: StatusCode, code: &'static str, title: &'static str, message: String) -> Self {
        Self {
            status,
            code,
            title,
            message,
            source: None,
            hints: vec![],
            show_routes: false,
            fields: None,
            sign_in_for: None,
            actions: vec![],
        }
    }

    pub fn hint(&mut self, hint: impl Into<String>) {
        self.hints.push(hint.into());
    }

    /// A report for responses that come from routing, not from an action.
    pub(crate) fn for_routing(status: StatusCode, method: &str, path: &str) -> Option<Self> {
        let mut report = match status {
            StatusCode::NOT_FOUND => {
                let mut r = Self::new(status, "K1001", "No route", format!("No route matches `{method} {path}`."));
                r.hint(format!(
                    "Declare one on an action: `#[action({method} \"{path}\")]` in `src/actions/<resource>/<verb>.rs`."
                ));
                r
            }
            StatusCode::METHOD_NOT_ALLOWED => {
                let mut r = Self::new(
                    status,
                    "K1004",
                    "Method not allowed",
                    format!("`{path}` exists, but not for `{method}`."),
                );
                r.hint("The `Allow` response header lists the methods this path accepts.");
                r
            }
            _ => return None,
        };
        report.show_routes = true;
        Some(report)
    }
}

#[derive(Debug, Clone)]
pub struct SourceExcerpt {
    pub file: String,
    pub line: usize,
    pub lang: &'static str,
    /// `(line number, text)` around `line`.
    pub lines: Vec<(usize, String)>,
}

impl SourceExcerpt {
    pub fn new(file: String, line: usize, source: &str, lang: &'static str) -> Self {
        let first = line.saturating_sub(3).max(1);
        let lines = source
            .lines()
            .enumerate()
            .map(|(i, text)| (i + 1, text.to_owned()))
            .filter(|(n, _)| *n >= first && *n <= line + 2)
            .collect();
        Self { file, line, lang, lines }
    }
}

/// The request an error happened in.
pub(crate) struct RequestInfo<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub action: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Format {
    Html,
    Markdown,
    Json,
}

/// Replaces `response`'s body with the rendered report, keeping status and headers.
pub(crate) fn render(response: Response, report: &ErrorReport, request: &RequestInfo<'_>, format: Format) -> Response {
    let detailed = cfg!(debug_assertions);
    let markdown = if detailed { markdown(report, request) } else { public_markdown(report) };
    let report_markdown = detailed.then(|| ReportMarkdown(markdown.clone()));
    let (body, content_type) = match format {
        Format::Json => (json(report, request, detailed), "application/problem+json"),
        Format::Markdown => (markdown, "text/markdown; charset=utf-8"),
        Format::Html if detailed => (html_page(report, request, &markdown), "text/html; charset=utf-8"),
        Format::Html => (public_html(report), "text/html; charset=utf-8"),
    };
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    parts.headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(report) = report_markdown {
        parts.extensions.insert(report);
    }
    Response::from_parts(parts, Body::from(body))
}

/// The development error report as Markdown, kept on the response whatever format it was
/// sent in, so in-process clients (tests, `kiln mcp`) can read it without asking twice.
#[derive(Clone)]
pub(crate) struct ReportMarkdown(pub String);

fn reason(status: StatusCode) -> &'static str {
    status.canonical_reason().unwrap_or("Error")
}

// ---------------------------------------------------------------------------------------
// Markdown

fn fence(text: &str) -> &'static str {
    if text.contains("````") {
        "`````"
    } else if text.contains("```") {
        "````"
    } else {
        "```"
    }
}

pub(crate) fn markdown(report: &ErrorReport, request: &RequestInfo<'_>) -> String {
    let mut md = String::new();
    let _ =
        writeln!(md, "# {} {} · {} ({})\n", report.status.as_u16(), reason(report.status), report.title, report.code);
    let _ = writeln!(md, "{}\n", report.message);
    let _ = writeln!(md, "- Request: `{} {}`", request.method, request.path);
    let _ =
        writeln!(md, "- Action: {}", request.action.map_or("none (no route matched)".to_owned(), |a| format!("`{a}`")));
    let _ = writeln!(md, "- Code: `{}` (`kiln explain {}` says more)\n", report.code, report.code);

    if let Some(source) = &report.source {
        let width = source.lines.last().map_or(1, |(n, _)| n.to_string().len());
        let body: String = source
            .lines
            .iter()
            .map(|(n, text)| {
                let marker = if *n == source.line { ">" } else { " " };
                format!("{marker} {n:>width$} | {text}\n")
            })
            .collect();
        let fence = fence(&body);
        let _ = writeln!(md, "## Source: `{}:{}`\n", source.file, source.line);
        let _ = writeln!(md, "{fence}{}\n{body}{fence}\n", source.lang);
    }

    if let Some(fields) = &report.fields {
        let _ = writeln!(md, "## Field errors\n");
        for (field, messages) in fields.iter() {
            for message in messages {
                let _ = writeln!(md, "- `{field}` {message}");
            }
        }
        md.push('\n');
    }

    if !report.hints.is_empty() {
        let _ = writeln!(md, "## How to fix\n");
        for hint in &report.hints {
            let _ = writeln!(md, "- {hint}");
        }
        for action in &report.actions {
            let _ = writeln!(
                md,
                "- Or run it from here: `POST {}` (the error page's \"{}\" button).",
                action.path, action.label
            );
        }
        md.push('\n');
    }

    if report.show_routes {
        let _ = writeln!(md, "## Routes\n\n| Method | Path | Action | Defined in |\n| --- | --- | --- | --- |");
        for route in routing::routes() {
            let _ = writeln!(
                md,
                "| {} | `{}` | `{}` | `{}:{}` |",
                route.method,
                route.path,
                route.name(),
                route.file,
                route.line
            );
        }
        md.push('\n');
    }
    md.push_str("_Kiln debug error report. Production responses show only the status._\n");
    md
}

/// RFC 9457 problem details. Release builds keep the message and field errors for client
/// errors (clients need them) and drop internals for server errors.
fn json(report: &ErrorReport, request: &RequestInfo<'_>, detailed: bool) -> String {
    let mut body = serde_json::json!({
        "type": format!("https://kiln.dev/errors/{}", report.code),
        "title": report.title,
        "status": report.status.as_u16(),
        "code": report.code,
    });
    if detailed || report.status.is_client_error() {
        body["detail"] = serde_json::Value::from(report.message.clone());
        if let Some(fields) = &report.fields {
            body["errors"] = serde_json::to_value(fields).unwrap_or_default();
        }
    }
    if detailed {
        body["instance"] = serde_json::Value::from(format!("{} {}", request.method, request.path));
        body["action"] = serde_json::Value::from(request.action);
        body["hints"] = serde_json::Value::from(report.hints.clone());
    }
    let mut text = serde_json::to_string_pretty(&body).expect("problem serializes");
    text.push('\n');
    text
}

fn public_markdown(report: &ErrorReport) -> String {
    format!("# {} {}\n", report.status.as_u16(), reason(report.status))
}

// ---------------------------------------------------------------------------------------
// HTML

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes text and renders `backtick spans` as `<code>`.
fn inline(s: &str) -> String {
    let mut out = String::new();
    for (i, part) in s.split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(out, "<code>{}</code>", esc(part));
        } else {
            out.push_str(&esc(part));
        }
    }
    out
}

fn html_page(report: &ErrorReport, request: &RequestInfo<'_>, markdown: &str) -> String {
    let severity = if report.status.is_server_error() { "server" } else { "client" };
    let status = report.status.as_u16();
    let mut h = String::with_capacity(16 * 1024);
    let _ = write!(
        h,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\">{FAVICON}\
         <title>{status} {title} · Kiln</title><style>{CSS}</style></head>\
         <body class=\"{severity}\"><div class=\"page\">",
        title = esc(report.title),
    );

    // Top bar
    let _ = write!(
        h,
        "<header class=\"bar\">\
           <div class=\"badges\">\
             <span class=\"status\">{status}</span>\
             <span class=\"title\">{title}</span>\
             <span class=\"code\" title=\"Stable error code: `kiln explain {code}` says more\">{code}</span>\
           </div>\
           <button id=\"copy\" class=\"copy\" type=\"button\" aria-describedby=\"copy-hint\">\
             <svg viewBox=\"0 0 16 16\" aria-hidden=\"true\"><rect x=\"5\" y=\"5\" width=\"8.5\" height=\"8.5\" rx=\"1.5\"/><path d=\"M10.5 3V2.5A1.5 1.5 0 0 0 9 1H3.5A1.5 1.5 0 0 0 2 2.5V8a1.5 1.5 0 0 0 1.5 1.5H4\"/></svg>\
             <span id=\"copy-label\">Copy as Markdown</span><kbd>C</kbd>\
           </button>\
         </header>",
        title = esc(report.title),
        code = esc(report.code),
    );

    // Message + request line
    let _ = write!(h, "<h1 class=\"message\">{}</h1>", inline(&report.message));
    let _ = write!(
        h,
        "<p class=\"request\"><span class=\"method\">{method}</span><span class=\"path\">{path}</span>\
         <span class=\"arrow\" aria-hidden=\"true\">→</span>{action}</p>",
        method = esc(request.method),
        path = esc(request.path),
        action = match request.action {
            Some(a) => format!("<span class=\"action\">{}</span>", esc(a)),
            None => "<span class=\"action none\">no route</span>".to_owned(),
        },
    );

    // Source excerpt
    if let Some(source) = &report.source {
        let _ = write!(
            h,
            "<section><h2>Source</h2><div class=\"source\"><div class=\"file\">{file}<span>:{line}</span></div><pre><code>",
            file = esc(&source.file),
            line = source.line,
        );
        for (n, text) in &source.lines {
            let class = if *n == source.line { " class=\"hit\"" } else { "" };
            let _ = write!(h, "<span{class}><i>{n}</i>{}\n</span>", esc(text));
        }
        h.push_str("</code></pre></div></section>");
    }

    // Hints
    if !report.hints.is_empty() {
        h.push_str("<section><h2>How to fix</h2><ul class=\"hints\">");
        for hint in &report.hints {
            let _ = write!(h, "<li>{}</li>", inline(hint));
        }
        h.push_str("</ul>");
        for action in &report.actions {
            let _ = write!(
                h,
                "<form class=\"resolve\" method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"_kiln\" value=\"{}\">\
                 <input type=\"hidden\" name=\"back\" value=\"{}\"><button>{}</button></form>",
                esc(&action.path),
                esc(&action.token),
                esc(&action.back),
                esc(&action.label)
            );
        }
        h.push_str("</section>");
    }

    // Routes
    if report.show_routes {
        h.push_str(
            "<section><h2>Routes</h2><div class=\"table\"><table><thead><tr><th>Method</th><th>Path</th>\
             <th>Action</th><th>Defined in</th></tr></thead><tbody>",
        );
        for route in routing::routes() {
            let _ = write!(
                h,
                "<tr><td><span class=\"method\">{}</span></td><td><code>{}</code></td><td>{}</td><td class=\"loc\">{}:{}</td></tr>",
                esc(route.method),
                esc(route.path),
                esc(route.name()),
                esc(route.file),
                route.line
            );
        }
        h.push_str("</tbody></table></div></section>");
    }

    // Footer
    let _ = write!(
        h,
        "<footer><p id=\"copy-hint\">Agents and scripts get this report as Markdown: request it without \
         <code>text/html</code> in <code>Accept</code>, e.g. <code>curl -H 'Accept: text/markdown' {path}</code>.</p>\
         <p>Debug build. In production visitors see only “{status} {reason}”.</p></footer>",
        path = esc(request.path),
        reason = reason(report.status),
    );

    let _ = write!(h, "<textarea id=\"kiln-md\" hidden readonly>{}</textarea>", esc(markdown));
    let _ = write!(h, "</div><script>{JS}</script></body></html>");
    h
}

fn public_html(report: &ErrorReport) -> String {
    let status = report.status.as_u16();
    let reason = reason(report.status);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         {FAVICON}<title>{status} {reason}</title><style>{PUBLIC_CSS}</style></head>\
         <body><main><p class=\"status\">{status}</p><h1>{reason}</h1></main></body></html>"
    )
}

const CSS: &str = r#"
:root {
  --bg: #f7f7f5; --panel: #ffffff; --ink: #17181a; --muted: #62666d; --rule: #e3e3df;
  --code-bg: #f1f1ee; --hit: #fff1ec; --hit-rule: #e5532d;
  --sev: #c2410c; --sev-soft: #fde9df;
  --sans: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  --mono: ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code", Menlo, Consolas, monospace;
  color-scheme: light;
}
body.client { --sev: #a16207; --sev-soft: #fbf0d4; --hit: #fff7e0; --hit-rule: #d29a10; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #0e0f11; --panel: #16171a; --ink: #ecedef; --muted: #9a9ea6; --rule: #26282d;
          --code-bg: #121316; --hit: #2a1712; --hit-rule: #f0714a; --sev: #f0714a; --sev-soft: #2e1811; color-scheme: dark; }
  body.client { --sev: #e8b339; --sev-soft: #2c230f; --hit: #282010; --hit-rule: #e8b339; }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body { margin: 0; background: var(--bg); color: var(--ink); font: 15px/1.55 var(--sans); }
body::before { content: ""; display: block; height: 3px; background: var(--sev); }
.page { max-width: 980px; margin: 0 auto; padding: 28px 24px 64px; }
.bar { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; justify-content: space-between; }
.badges { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
.status { font: 600 13px/1 var(--mono); color: var(--sev); background: var(--sev-soft); padding: 6px 9px; border-radius: 6px; }
.title { font-weight: 600; }
.code { font: 12px/1 var(--mono); color: var(--muted); border: 1px solid var(--rule); padding: 5px 7px; border-radius: 6px; }
.copy { display: inline-flex; gap: 8px; align-items: center; font: 500 13px/1 var(--sans); color: var(--ink);
        background: var(--panel); border: 1px solid var(--rule); border-radius: 8px; padding: 8px 10px; cursor: pointer; }
.copy:hover { border-color: var(--muted); }
.copy:focus-visible { outline: 2px solid var(--sev); outline-offset: 2px; }
.copy svg { width: 15px; height: 15px; fill: none; stroke: currentColor; stroke-width: 1.3; }
.copy kbd { font: 11px/1 var(--mono); color: var(--muted); border: 1px solid var(--rule); border-radius: 4px; padding: 2px 5px; }
.copy.done { color: var(--sev); border-color: var(--sev); }
.message { font: 600 clamp(22px, 3.4vw, 30px)/1.25 var(--sans); letter-spacing: -0.01em; margin: 28px 0 10px; text-wrap: balance; overflow-wrap: anywhere; }
.request { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 0 0 8px; font: 13px/1.4 var(--mono); color: var(--muted); }
.method { font: 600 11px/1 var(--mono); letter-spacing: .04em; color: var(--ink); border: 1px solid var(--rule); background: var(--panel); padding: 4px 6px; border-radius: 4px; }
.path { color: var(--ink); overflow-wrap: anywhere; }
.action { color: var(--ink); }
.action.none { color: var(--sev); }
section { margin-top: 32px; }
h2 { font: 600 11px/1 var(--mono); text-transform: uppercase; letter-spacing: .1em; color: var(--muted); margin: 0 0 10px; }
code { font: 0.92em var(--mono); background: var(--code-bg); border: 1px solid var(--rule); padding: 1px 5px; border-radius: 4px; }
.source { border: 1px solid var(--rule); border-radius: 10px; overflow: hidden; background: var(--panel); }
.file { font: 12.5px/1 var(--mono); padding: 10px 14px; border-bottom: 1px solid var(--rule); color: var(--ink); }
.file span { color: var(--sev); }
.source pre { margin: 0; overflow-x: auto; background: var(--code-bg); }
.source pre code { display: block; font: 13px/1.7 var(--mono); background: none; border: 0; padding: 10px 0; border-radius: 0; }
.source pre span { display: block; padding: 0 14px 0 0; white-space: pre; }
.source pre i { display: inline-block; width: 3.5em; padding-right: 1em; text-align: right; font-style: normal; color: var(--muted); user-select: none; }
.source pre .hit { background: var(--hit); box-shadow: inset 3px 0 0 var(--hit-rule); }
.source pre .hit i { color: var(--hit-rule); }
.hints { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
.hints li { background: var(--panel); border: 1px solid var(--rule); border-radius: 10px; padding: 12px 14px; max-width: 78ch; }
.resolve { margin: 12px 0 0; }
.resolve button { font: 600 14px/1 var(--sans); color: var(--panel); background: var(--sev); border: 0; border-radius: 8px; padding: 10px 16px; cursor: pointer; }
.resolve button:hover { filter: brightness(1.08); }
.table { overflow-x: auto; border: 1px solid var(--rule); border-radius: 10px; background: var(--panel); }
table { border-collapse: collapse; width: 100%; font-size: 13.5px; }
th, td { text-align: left; padding: 9px 14px; border-bottom: 1px solid var(--rule); white-space: nowrap; }
tr:last-child td { border-bottom: 0; }
th { font: 600 11px/1 var(--mono); text-transform: uppercase; letter-spacing: .08em; color: var(--muted); }
td.loc { font: 12px var(--mono); color: var(--muted); }
footer { margin-top: 40px; padding-top: 16px; border-top: 1px solid var(--rule); color: var(--muted); font-size: 13px; }
footer p { margin: 0 0 6px; }
@media (max-width: 560px) { .page { padding: 20px 16px 48px; } .copy kbd { display: none; } }
"#;

/// The Kiln mark (`assets/brand/favicon.svg`), so error tabs are recognizable.
const FAVICON: &str = "<link rel=\"icon\" href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Cstyle%3E.s{fill:%23a3401b}@media (prefers-color-scheme:dark){.s{fill:%23f2a35e}}%3C/style%3E%3Cpath class='s' d='M10 7h12v50H10Z'/%3E%3Cpath d='M42 7h14L39 32l17 25H42L27 32Z' fill='%23e8772e'/%3E%3C/svg%3E\">";

const PUBLIC_CSS: &str = r#"
:root { color-scheme: light dark; }
body { margin: 0; min-height: 100vh; display: grid; place-items: center; font: 16px/1.5 ui-sans-serif, system-ui, sans-serif; background: Canvas; color: CanvasText; }
main { text-align: center; padding: 24px; }
.status { font: 600 14px ui-monospace, Menlo, monospace; opacity: .6; margin: 0; }
h1 { font-size: 28px; margin: 8px 0 0; }
"#;

const JS: &str = r#"
(function () {
  var button = document.getElementById('copy');
  var label = document.getElementById('copy-label');
  var source = document.getElementById('kiln-md');
  function done(text) {
    label.textContent = text;
    button.classList.add('done');
    setTimeout(function () { label.textContent = 'Copy as Markdown'; button.classList.remove('done'); }, 1600);
  }
  function fallback(md) {
    var area = document.createElement('textarea');
    area.value = md; area.style.position = 'fixed'; area.style.opacity = '0';
    document.body.appendChild(area); area.select();
    try { document.execCommand('copy'); done('Copied'); } catch (e) { done('Select and copy manually'); }
    area.remove();
  }
  function copy() {
    var md = source.value;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(md).then(function () { done('Copied'); }, function () { fallback(md); });
    } else { fallback(md); }
  }
  button.addEventListener('click', copy);
  document.addEventListener('keydown', function (e) {
    if ((e.key === 'c' || e.key === 'C') && !e.metaKey && !e.ctrlKey && !e.altKey && !String(window.getSelection())) copy();
  });
})();
"#;
