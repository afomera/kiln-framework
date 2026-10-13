//! `/kiln`: the command center. Open in development; in production only for viewers the
//! app allows with `App::kiln_tools_for` (everyone else gets a 404).
//!
//! `/kiln/dashboard` links everything; each tool is a page: routes, records, jobs,
//! schedules, batches, mail, broadcasts and sign-ins. Jobs, queues, schedules and batches
//! have controls (retry, cancel, pause, run now). Their forms post a token derived from the
//! app key and the viewer's sign-in, which other sites can't read, and cross-site requests
//! are refused.

use std::collections::HashMap;

use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use sqlx::Row;

use crate::{Db, migrate::records, routes, view::escape_attribute as esc};

/// Lucide icons (ISC license, lucide.dev), inlined so the pages load nothing external.
mod icon {
    pub const DASHBOARD: &str = r#"<rect width="7" height="9" x="3" y="3" rx="1"/><rect width="7" height="5" x="14" y="3" rx="1"/><rect width="7" height="9" x="14" y="12" rx="1"/><rect width="7" height="5" x="3" y="16" rx="1"/>"#;
    pub const ROUTE: &str = r#"<circle cx="6" cy="19" r="3"/><path d="M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15"/><circle cx="18" cy="5" r="3"/>"#;
    pub const DATABASE: &str = r#"<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5V19A9 3 0 0 0 21 19V5"/><path d="M3 12A9 3 0 0 0 21 12"/>"#;
    pub const JOBS: &str =
        r#"<path d="M16 20V4a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16"/><rect width="20" height="14" x="2" y="6" rx="2"/>"#;
    pub const MAIL: &str =
        r#"<rect width="20" height="16" x="2" y="4" rx="2"/><path d="m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7"/>"#;
    pub const RADIO: &str = r#"<path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.1 19.1 19"/>"#;
    pub const KEY: &str = r#"<path d="M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z"/><circle cx="16.5" cy="7.5" r=".5" fill="currentColor"/>"#;
    pub const TERMINAL: &str = r#"<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>"#;
    pub const CALENDAR: &str =
        r#"<path d="M8 2v4"/><path d="M16 2v4"/><rect width="18" height="18" x="3" y="4" rx="2"/><path d="M3 10h18"/>"#;
    pub const LAYERS: &str = r#"<path d="m12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/><path d="m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65"/><path d="m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65"/>"#;
    pub const BELL: &str = r#"<path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/>"#;
    pub const ARROW: &str = r#"<path d="M5 12h14"/><path d="m12 5 7 7-7 7"/>"#;
}

fn svg(paths: &str) -> String {
    format!(
        r#"<svg class="i" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{paths}</svg>"#
    )
}

/// The tools, in nav order: `(slug, title, icon)`.
const TOOLS: &[(&str, &str, &str)] = &[
    ("dashboard", "Dashboard", icon::DASHBOARD),
    ("routes", "Routes", icon::ROUTE),
    ("records", "Records", icon::DATABASE),
    ("jobs", "Jobs", icon::JOBS),
    ("schedules", "Schedules", icon::CALENDAR),
    ("batches", "Batches", icon::LAYERS),
    ("mail", "Mail", icon::MAIL),
    ("broadcasts", "Broadcasts", icon::RADIO),
    ("notifications", "Notifications", icon::BELL),
    ("sign_ins", "Sign-ins", icon::KEY),
];

/// A dev tools page: the shared shell, nav and styles around `body`.
pub(crate) fn page(active: &str, title: &str, body: &str) -> Response {
    page_with_notice(active, title, body, None)
}

fn page_with_notice(active: &str, title: &str, body: &str, notice: Option<&str>) -> Response {
    let notice = notice.map(|n| format!("<p class=notice>{}</p>", esc(n))).unwrap_or_default();
    let nav: String = TOOLS
        .iter()
        .map(|(slug, name, paths)| {
            let on = if *slug == active { " aria-current=\"page\"" } else { "" };
            format!("<a href=\"/kiln/{slug}\"{on}>{}{name}</a>", svg(paths))
        })
        .collect();
    Html(format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} · Kiln</title><style>{STYLE}</style>
<meta name="turbo-refresh-method" content="morph"><meta name="turbo-refresh-scroll" content="preserve">
<script type="module" src="{turbo}"></script></head><body>
<turbo-stream-source src="/kiln/live?page={active}"></turbo-stream-source>
<header><a class="brand" href="/kiln/dashboard"><svg viewBox="0 0 64 64" aria-hidden="true"><path d="M10 7h12v50H10Z" fill="var(--accent)"/><path d="M42 7h14L39 32l17 25H42L27 32Z" fill="#e8772e"/></svg>Kiln <span>{env}</span></a>
<nav>{nav}</nav>{viewer}<button class=live type=button title="Pages refresh as jobs, mail and the rest change" onclick="const s=document.querySelector('turbo-stream-source');if(s){{s.remove();this.textContent='○ Paused'}}else{{location.reload()}}">● Live</button><a class="app" href="/">Open the app{arrow}</a></header>
<main><h1>{title}</h1>{notice}{body}</main></body></html>"##,
        title = esc(title),
        arrow = svg(icon::ARROW),
        env = crate::config::env(),
        turbo = crate::assets::turbo_url(),
        viewer = if crate::config::is_production() {
            VIEWER.try_with(|v| format!("<span class=muted>{}</span>", esc(&v.who))).unwrap_or_default()
        } else {
            String::new()
        },
    ))
    .into_response()
}

const STYLE: &str = r#"
:root{--bg:#f6f5f4;--surface:#fff;--ink:#1c1917;--muted:#6f6863;--rule:#e7e5e4;--accent:#a3401b;--soft:#f6e6dd;--ok:#2f6b3f;--warn:#8a5a00;--bad:#8a2c2c;--code:#f1efee;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#121110;--surface:#1b1918;--ink:#ece8e4;--muted:#a39d97;--rule:#2d2a28;--accent:#ec8a5e;--soft:#3a2219;--ok:#7cc48e;--warn:#e3c07a;--bad:#e58a8a;--code:#24211f;color-scheme:dark}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
a{color:var(--accent)}code,pre{font:13px/1.5 ui-monospace,"SF Mono",Menlo,monospace}code{background:var(--code);padding:1px 5px;border-radius:4px}
header{display:flex;flex-wrap:wrap;align-items:center;gap:8px 20px;padding:12px 24px;background:var(--surface);border-bottom:1px solid var(--rule);position:sticky;top:0}
.brand{display:flex;align-items:center;gap:8px;font-weight:700;color:var(--ink);text-decoration:none}.brand svg{width:22px;height:22px}.brand span{font-weight:400;color:var(--muted);font-size:13px}
nav{display:flex;flex-wrap:wrap;gap:2px;flex:1}nav a,.app{display:flex;align-items:center;gap:6px;padding:6px 10px;border-radius:6px;color:var(--muted);text-decoration:none;font-size:14px}
nav a:hover,nav a[aria-current]{background:var(--soft);color:var(--ink)}.app{color:var(--accent)}
.i{width:16px;height:16px;flex:none}main{max-width:1100px;margin:0 auto;padding:24px 24px 64px}h1{font-size:26px;margin:8px 0 20px;letter-spacing:-.01em}h2{display:flex;align-items:center;gap:8px;flex-wrap:wrap;font-size:17px;margin:28px 0 10px}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:12px}
.card{display:block;background:var(--surface);border:1px solid var(--rule);border-radius:10px;padding:16px;color:inherit;text-decoration:none}.card:hover{border-color:var(--accent)}
.card .t{display:flex;align-items:center;gap:8px;font-weight:600}.card .t .i{width:18px;height:18px;color:var(--accent)}.card .n{font-size:28px;font-weight:700;margin:6px 0 2px}.card p{margin:0;color:var(--muted);font-size:13.5px}
.tbl{overflow-x:auto;background:var(--surface);border:1px solid var(--rule);border-radius:10px}table{border-collapse:collapse;width:100%;font-size:14px}
th,td{text-align:left;vertical-align:top;padding:8px 12px;border-bottom:1px solid var(--rule)}tr:last-child td{border-bottom:0}th{font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);font-weight:500}
.pill{display:inline-block;font-size:12px;padding:1px 8px;border-radius:99px;border:1px solid currentColor}.queued,.GET{color:var(--ok)}.running,.POST,.PATCH{color:var(--warn)}.failed,.DELETE{color:var(--bad)}
.muted,.empty{color:var(--muted)}pre{margin:0;white-space:pre-wrap;word-break:break-word;max-height:160px;overflow:auto}
.row{display:flex;justify-content:space-between;gap:1rem;padding:.7rem 1rem;border:1px solid var(--rule);border-radius:8px;margin:.5rem 0;background:var(--surface);text-decoration:none;color:inherit}.row span{color:var(--muted)}
iframe{width:100%;height:480px;border:1px solid var(--rule);border-radius:8px;background:#fff}dl{display:grid;grid-template-columns:auto 1fr;gap:.2rem 1rem}dt{color:var(--muted)}dd{margin:0}
.filter{display:flex;gap:8px;align-items:center;margin:0 0 12px;flex-wrap:wrap}.filter input{font:inherit;padding:5px 10px;border:1px solid var(--rule);border-radius:6px;background:var(--surface);color:var(--ink);min-width:min(320px,100%)}
button.live{border:0;background:none;color:var(--ok);font-size:13px}
.notice{background:var(--soft);border-radius:8px;padding:8px 12px}
form.inline{display:inline;margin:0}button{font:inherit;font-size:13px;padding:3px 10px;border-radius:6px;border:1px solid var(--rule);background:var(--surface);color:var(--ink);cursor:pointer}
button:hover{border-color:var(--accent)}button.danger{color:var(--bad)}.actions{display:flex;gap:6px;flex-wrap:wrap}
.tabs{display:flex;gap:4px;flex-wrap:wrap;margin:0 0 12px}.tabs a{padding:4px 10px;border-radius:99px;border:1px solid var(--rule);text-decoration:none;color:var(--muted);font-size:13px}.tabs a[aria-current]{background:var(--soft);color:var(--ink);border-color:var(--accent)}
.bar{display:flex;height:8px;border-radius:99px;overflow:hidden;background:var(--rule);min-width:120px}.bar .s{background:var(--ok)}.bar .f{background:var(--bad)}.bar .c{background:var(--muted)}
.cmds{display:grid;gap:6px}.cmds div{display:flex;gap:10px;align-items:baseline}.cmds code{white-space:nowrap}
@media (max-width:640px){header{padding:10px 16px}main{padding:16px}}
"#;

/// Serves `/kiln` pages, or `None` for other paths (assets, streams).
pub(crate) async fn serve(ctx: &crate::Ctx, viewer: Viewer, path: &str, query: Option<&str>) -> Option<Response> {
    VIEWER.scope(viewer, serve_page(ctx, path, query)).await
}

async fn serve_page(ctx: &crate::Ctx, path: &str, query: Option<&str>) -> Option<Response> {
    let db = &ctx.db;
    let query: Query = query.and_then(|q| serde_urlencoded::from_str(q).ok()).unwrap_or_default();
    let notice = query.get("done").map(String::as_str);
    let trimmed = path.trim_end_matches('/');
    if let Some(id) = trimmed.strip_prefix("/kiln/batches/").and_then(|id| id.parse().ok()) {
        return Some(batch_page(db, id, notice).await.unwrap_or_else(IntoResponse::into_response));
    }
    let result = match trimmed {
        "/kiln" => return Some((StatusCode::SEE_OTHER, [(header::LOCATION, "/kiln/dashboard")]).into_response()),
        "/kiln/dashboard" => dashboard(db).await,
        "/kiln/routes" => Ok(routes_page(&query)),
        "/kiln/records" => records_page(db, &query).await,
        "/kiln/jobs" => jobs_page(db, &query, notice).await,
        "/kiln/schedules" => schedules_page(db, &query, notice).await,
        "/kiln/batches" => batches_page(db, &query, notice).await,
        "/kiln/broadcasts" => broadcasts_page(db, &query).await,
        "/kiln/sign_ins" => sign_ins_page(db, &query).await,
        "/kiln/notifications" => notifications_page(db, &query).await,
        "/kiln/live" => return Some(live(db.clone(), param(&query, "page").to_owned())),
        p if p == "/kiln/mail" || p.starts_with("/kiln/mail/") => {
            return crate::mail::serve_outbox(ctx, path, &query).await;
        }
        _ => return None,
    };
    Some(result.unwrap_or_else(IntoResponse::into_response))
}

async fn count(db: &Db, sql: &str) -> crate::Result<i64> {
    Ok(db.fetch_all(sql, vec![]).await?.first().map(|r| r.get(0)).unwrap_or(0))
}

fn when(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds).map(|t| t.strftime("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

/// A page's query string: `q` (search) plus page-specific filters.
pub(crate) type Query = HashMap<String, String>;

fn param<'a>(query: &'a Query, key: &str) -> &'a str {
    query.get(key).map(|v| v.trim()).unwrap_or("")
}

/// Whether every word of the search appears in one of `fields`, ignoring case.
pub(crate) fn matches(query: &Query, fields: &[&str]) -> bool {
    let search = param(query, "q").to_lowercase();
    let haystack: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    search.split_whitespace().all(|word| haystack.iter().any(|f| f.contains(word)))
}

/// `path?key=value&..`, skipping empty values.
fn link(path: &str, params: &[(&str, &str)]) -> String {
    let params: Vec<(&str, &str)> = params.iter().copied().filter(|(_, v)| !v.is_empty() && *v != "all").collect();
    if params.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", serde_urlencoded::to_string(&params).unwrap_or_default())
    }
}

/// Pill links for one filter (`key`), keeping the others in `keep`.
fn tabs(path: &str, query: &Query, key: &str, options: &[(&str, &str)], keep: &[&str]) -> String {
    let current = match param(query, key) {
        "" => "all",
        v => v,
    };
    let links: String = options
        .iter()
        .map(|(value, label)| {
            let mut params: Vec<(&str, &str)> = keep.iter().map(|k| (*k, param(query, k))).collect();
            params.push((key, value));
            let on = if *value == current { " aria-current=page" } else { "" };
            format!("<a href=\"{}\"{on}>{label}</a>", esc(&link(path, &params)))
        })
        .collect();
    format!("<div class=tabs>{links}</div>")
}

/// A search box that keeps the filters in `keep`.
pub(crate) fn search_bar(path: &str, query: &Query, placeholder: &str, keep: &[&str]) -> String {
    let hidden: String = keep
        .iter()
        .filter(|k| !param(query, k).is_empty())
        .map(|k| format!("<input type=hidden name=\"{k}\" value=\"{}\">", esc(param(query, k))))
        .collect();
    let clear = if param(query, "q").is_empty() {
        String::new()
    } else {
        let params: Vec<(&str, &str)> = keep.iter().map(|k| (*k, param(query, k))).collect();
        format!("<a href=\"{}\">Clear</a>", esc(&link(path, &params)))
    };
    format!(
        "<form class=filter method=get action=\"{path}\"><input type=search id=kiln-search data-turbo-permanent name=q value=\"{}\" placeholder=\"{}\">{hidden}<button>Search</button>{clear}</form>",
        esc(param(query, "q")),
        esc(placeholder)
    )
}

/// "Showing 3 of 12" when a filter hides some.
pub(crate) fn showing(shown: usize, total: usize) -> String {
    if shown == total { String::new() } else { format!("<p class=muted>Showing {shown} of {total}.</p>") }
}

async fn dashboard(db: &Db) -> crate::Result<Response> {
    let routes = routes();
    let records = records();
    let jobs = db.job_counts().await?;
    let job_count = |status: &str| jobs.iter().find(|(s, _)| s == status).map_or(0, |(_, n)| *n);
    let mail = count(db, "SELECT COUNT(*) FROM \"kiln_mail\"").await?;
    let broadcasts = count(db, "SELECT COUNT(*) FROM \"kiln_stream_messages\"").await?;
    let streams = count(db, "SELECT COUNT(DISTINCT \"stream\") FROM \"kiln_stream_messages\"").await?;
    let sign_ins = count(db, "SELECT COUNT(*) FROM \"kiln_sessions\"").await?;
    let unread = count(db, "SELECT COUNT(*) FROM \"kiln_notifications\" WHERE \"read_at\" IS NULL").await?;
    let schedules = db.schedules().await?;
    let running_batches = count(db, "SELECT COUNT(*) FROM \"kiln_batches\" WHERE \"status\" = 'running'").await?;
    let card = |slug: &str, icon: &str, title: &str, n: String, detail: String| {
        format!(
            "<a class=card href=\"/kiln/{slug}\"><div class=t>{}{title}</div><div class=n>{n}</div><p>{detail}</p></a>",
            svg(icon)
        )
    };
    let failed = job_count("failed");
    let cards = [
        card("routes", icon::ROUTE, "Routes", routes.len().to_string(), "Every action, with its file".into()),
        card("records", icon::DATABASE, "Records", records.len().to_string(), "Tables, columns and row counts".into()),
        card(
            "jobs",
            icon::JOBS,
            "Jobs",
            (job_count("queued") + job_count("running")).to_string(),
            if failed > 0 {
                format!("waiting or running · <b>{failed} failed</b>")
            } else {
                "waiting or running · none failed".into()
            },
        ),
        card(
            "schedules",
            icon::CALENDAR,
            "Schedules",
            schedules.len().to_string(),
            match schedules.iter().filter(|s| s.paused).count() {
                0 => "recurring jobs · none paused".into(),
                n => format!("recurring jobs · <b>{n} paused</b>"),
            },
        ),
        card("batches", icon::LAYERS, "Batches", running_batches.to_string(), "running, with completion hooks".into()),
        card("mail", icon::MAIL, "Mail", mail.to_string(), "In the development outbox".into()),
        card("notifications", icon::BELL, "Notifications", unread.to_string(), "unread, across every inbox".into()),
        card(
            "broadcasts",
            icon::RADIO,
            "Broadcasts",
            broadcasts.to_string(),
            format!("in the log, across {streams} stream(s)"),
        ),
        card("sign_ins", icon::KEY, "Sign-ins", sign_ins.to_string(), "Active sessions, every account type".into()),
    ]
    .concat();
    let commands = [
        ("kiln check", "compile, conventions, schema drift"),
        ("kiln test", "the app's tests"),
        ("kiln routes", "the route table"),
        ("kiln db status", "pending migrations and drift"),
        ("kiln jobs retry", "queue failed jobs again"),
        ("kiln secrets show", "decrypted secrets"),
    ]
    .iter()
    .map(|(c, d)| format!("<div><code>{c}</code><span class=muted>{d}</span></div>"))
    .collect::<String>();
    let body = format!(
        "<div class=cards>{cards}</div><h2>{}Commands</h2><div class=cmds>{commands}</div>\
         <p class=muted>Environment <code>{}</code> · in production these pages are only for viewers <code>app.kiln_tools_for(..)</code> allows.</p>",
        svg(icon::TERMINAL),
        crate::config::env()
    );
    Ok(page("dashboard", "Dashboard", &body))
}

fn routes_page(query: &Query) -> Response {
    let all = routes();
    let method = param(query, "method");
    let pipeline = param(query, "pipeline");
    let shown: Vec<_> = all
        .iter()
        .filter(|r| method.is_empty() || method == "all" || r.method == method)
        .filter(|r| pipeline.is_empty() || pipeline == "all" || r.pipeline == pipeline)
        .filter(|r| matches(query, &[r.path, &r.name(), r.file]))
        .collect();
    let rows: String = shown
        .iter()
        .map(|r| {
            format!(
                "<tr><td><span class=\"pill {m}\">{m}</span></td><td><code>{}</code></td><td>{}</td><td class=muted>{}:{}</td><td class=muted>{}</td></tr>",
                esc(r.path),
                esc(&r.name()),
                esc(r.file),
                r.line,
                r.pipeline,
                m = r.method,
            )
        })
        .collect();
    let body = format!(
        "{}{}{}{}<div class=tbl><table><thead><tr><th>Method</th><th>Path</th><th>Action</th><th>File</th><th>Pipeline</th></tr></thead><tbody>{rows}</tbody></table></div>",
        search_bar("/kiln/routes", query, "Path, action or file", &["method", "pipeline"]),
        tabs(
            "/kiln/routes",
            query,
            "method",
            &[("all", "Any method"), ("GET", "GET"), ("POST", "POST"), ("PATCH", "PATCH"), ("DELETE", "DELETE")],
            &["q", "pipeline"]
        ),
        tabs(
            "/kiln/routes",
            query,
            "pipeline",
            &[("all", "Any pipeline"), ("browser", "Browser"), ("api", "API")],
            &["q", "method"]
        ),
        showing(shown.len(), all.len()),
    );
    page("routes", "Routes", &body)
}

async fn records_page(db: &Db, query: &Query) -> crate::Result<Response> {
    let all = records();
    let mut body = search_bar("/kiln/records", query, "Record, table or column", &[]);
    let mut shown = 0;
    for record in &all {
        let schema = (record.schema)();
        let names: Vec<&str> = schema.columns.iter().map(|c| c.name).collect();
        let mut fields = vec![record.name, record.table];
        fields.extend(names);
        if !matches(query, &fields) {
            continue;
        }
        shown += 1;
        let rows = count(db, &format!("SELECT COUNT(*) FROM \"{}\"", record.table)).await.unwrap_or(-1);
        let columns: String = schema
            .columns
            .iter()
            .map(|c| {
                let mut notes = vec![];
                if c.primary_key {
                    notes.push("primary key".to_owned());
                }
                if c.unique {
                    notes.push("unique".to_owned());
                } else if c.indexed {
                    notes.push("indexed".to_owned());
                }
                if let Some(target) = c.references {
                    notes.push(format!("→ {target}"));
                }
                if !c.variants.is_empty() {
                    notes.push(c.variants.join(" | "));
                }
                format!(
                    "<tr><td><code>{}</code></td><td>{}{}</td><td class=muted>{}</td></tr>",
                    c.name,
                    c.sql_type,
                    if c.nullable { "?" } else { "" },
                    esc(&notes.join(", "))
                )
            })
            .collect();
        let rows = if rows < 0 {
            "table missing: run <code>kiln db migrate</code>".to_owned()
        } else {
            format!("{rows} row(s)")
        };
        body.push_str(&format!(
            "<h2>{} <span class=muted>· <code>{}</code> · {rows}</span></h2><div class=tbl><table><thead><tr><th>Column</th><th>Type</th><th>Notes</th></tr></thead><tbody>{columns}</tbody></table></div>",
            record.name, record.table
        ));
    }
    body.push_str(&showing(shown, all.len()));
    Ok(page("records", "Records", &body))
}

/// Who is looking at the tools, and the token their control forms post.
#[derive(Clone)]
pub(crate) struct Viewer {
    who: String,
    token: String,
}

impl Viewer {
    /// `seed` is the viewer's sign-in token (or a fixed string in development), so the
    /// token is per session and the same in every process.
    pub(crate) fn new(key: &crate::SecretKey, who: String, seed: &str) -> Self {
        let signed = key.sign_str(&format!("kiln-tools:{seed}"));
        let token = signed.rsplit('.').next().unwrap_or_default().to_owned();
        Self { who, token }
    }
}

tokio::task_local! {
    static VIEWER: Viewer;
}

/// The token every control form posts.
fn token() -> String {
    VIEWER.try_with(|v| v.token.clone()).unwrap_or_default()
}

/// A one-button form that posts to `action` and comes back to `back`.
fn button(action: &str, label: &str, back: &str, danger: bool) -> String {
    format!(
        "<form class=inline method=post action=\"{}\"><input type=hidden name=_kiln value=\"{}\"><input type=hidden name=back value=\"{}\"><button{}>{}</button></form>",
        esc(action),
        token(),
        esc(back),
        if danger { " class=danger" } else { "" },
        esc(label)
    )
}

async fn jobs_page(db: &Db, query: &HashMap<String, String>, notice: Option<&str>) -> crate::Result<Response> {
    let status = query.get("status").map(String::as_str).unwrap_or("all");
    let queue = query.get("queue").cloned();
    let search = param(query, "q").to_owned();
    let back = link("/kiln/jobs", &[("status", status), ("queue", queue.as_deref().unwrap_or("")), ("q", &search)]);
    let now = jiff::Timestamp::now().as_microsecond();

    // Queues, with pause controls.
    let limits: HashMap<String, i64> = db
        .fetch_all("SELECT \"queue\", \"max_running\" FROM \"kiln_queue_limits\"", vec![])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let queues: String = db
        .queues()
        .await?
        .iter()
        .map(|(q, paused, queued, running, failed)| {
            let limit = limits.get(q).map_or_else(|| "<span class=muted>workers</span>".to_owned(), |n| format!("{n} at a time"));
            let control = if *paused {
                button(&format!("/kiln/queues/{q}/resume"), "Resume", &back, false)
            } else {
                button(&format!("/kiln/queues/{q}/pause"), "Pause", &back, false)
            };
            format!(
                "<tr><td><a href=\"/kiln/jobs?queue={q}\"><code>{q}</code></a>{}</td><td>{queued}</td><td>{running}</td><td>{failed}</td><td>{limit}</td><td>{control}</td></tr>",
                if *paused { " <span class=\"pill failed\">paused</span>" } else { "" },
                q = esc(q),
            )
        })
        .collect();
    let queues = if queues.is_empty() {
        String::new()
    } else {
        format!(
            "<h2>Queues</h2><div class=tbl><table><thead><tr><th>Queue</th><th>Waiting</th><th>Running</th><th>Failed</th><th>Concurrency</th><th></th></tr></thead><tbody>{queues}</tbody></table></div>"
        )
    };

    let status_tabs = tabs(
        "/kiln/jobs",
        query,
        "status",
        &[
            ("all", "All"),
            ("queued", "Waiting"),
            ("scheduled", "Delayed"),
            ("running", "Running"),
            ("failed", "Failed"),
        ],
        &["q", "queue"],
    );
    let queue_names: Vec<String> = db.queues().await?.into_iter().map(|(q, ..)| q).collect();
    let queue_options: Vec<(&str, &str)> =
        std::iter::once(("all", "Any queue")).chain(queue_names.iter().map(|q| (q.as_str(), q.as_str()))).collect();
    let queue_tabs = if queue_names.len() > 1 {
        tabs("/kiln/jobs", query, "queue", &queue_options, &["q", "status"])
    } else {
        String::new()
    };
    let mut conditions = vec![];
    let mut binds = vec![];
    match status {
        "queued" => {
            conditions.push("\"status\" = 'queued' AND \"run_at\" <= ?");
            binds.push(crate::Value::Integer(now));
        }
        "scheduled" => {
            conditions.push("\"status\" = 'queued' AND \"run_at\" > ?");
            binds.push(crate::Value::Integer(now));
        }
        "running" => conditions.push("\"status\" = 'running'"),
        "failed" => conditions.push("\"status\" = 'failed'"),
        _ => {}
    }
    if let Some(queue) = queue.as_deref().filter(|q| !q.is_empty() && *q != "all") {
        conditions.push("\"queue\" = ?");
        binds.push(crate::Value::Text(queue.to_owned()));
    }
    for word in search.split_whitespace() {
        conditions.push("(\"name\" LIKE ? OR \"payload\" LIKE ? OR COALESCE(\"last_error\", '') LIKE ? OR CAST(\"id\" AS TEXT) = ?)");
        let like = format!("%{word}%");
        binds.extend([
            crate::Value::Text(like.clone()),
            crate::Value::Text(like.clone()),
            crate::Value::Text(like),
            crate::Value::Text(word.trim_start_matches('#').to_owned()),
        ]);
    }
    let filter = if conditions.is_empty() { String::new() } else { format!("WHERE {}", conditions.join(" AND ")) };
    let rows = db
        .fetch_all(
            &format!(
                "SELECT \"id\", \"name\", \"queue\", \"status\", \"attempts\", \"max_attempts\", \"run_at\", COALESCE(\"last_error\", ''), \"payload\", \"batch_id\", \"priority\", \"cancel_requested\" \
                 FROM \"kiln_jobs\" {filter} ORDER BY CASE \"status\" WHEN 'failed' THEN 0 WHEN 'running' THEN 1 ELSE 2 END, \"priority\" DESC, \"run_at\" LIMIT 200"
            ),
            binds,
        )
        .await?;
    let rows: String = rows
        .iter()
        .map(|r| {
            let (id, status, run_at): (i64, String, i64) = (r.get(0), r.get(3), r.get(6));
            let delayed = status == "queued" && run_at > now;
            let shown = if delayed { "delayed" } else { status.as_str() };
            let mut actions = vec![];
            match status.as_str() {
                "failed" => {
                    actions.push(button(&format!("/kiln/jobs/{id}/retry"), "Retry", &back, false));
                    actions.push(button(&format!("/kiln/jobs/{id}/cancel"), "Discard", &back, true));
                }
                "queued" => {
                    if delayed {
                        actions.push(button(&format!("/kiln/jobs/{id}/run"), "Run now", &back, false));
                    }
                    actions.push(button(&format!("/kiln/jobs/{id}/cancel"), "Cancel", &back, true));
                }
                _ if r.get::<i64, _>(11) != 0 => actions.push("<span class=muted>stopping…</span>".to_owned()),
                _ => actions.push(button(&format!("/kiln/jobs/{id}/cancel"), "Stop", &back, true)),
            }
            let priority = match r.get::<i64, _>(10) {
                0 => String::new(),
                p => format!(" <span class=muted>priority {p}</span>"),
            };
            let batch = r.get::<Option<i64>, _>(9).map(|b| format!(" <a href=\"/kiln/batches/{b}\">batch #{b}</a>")).unwrap_or_default();
            format!(
                "<tr><td>#{id}</td><td><b>{}</b>{priority}{batch}<pre class=muted>{}</pre></td><td>{}</td><td><span class=\"pill {status}\">{shown}</span></td><td>{}/{}</td><td>{}</td><td><pre>{}</pre></td><td><div class=actions>{}</div></td></tr>",
                esc(&r.get::<String, _>(1)),
                esc(&r.get::<String, _>(8)),
                esc(&r.get::<String, _>(2)),
                r.get::<i64, _>(4),
                r.get::<i64, _>(5),
                when(run_at / 1_000_000),
                esc(&r.get::<String, _>(7)),
                actions.concat(),
            )
        })
        .collect();
    let table = if rows.is_empty() {
        "<p class=empty>No jobs here. Finished jobs are deleted; waiting, delayed, running and failed ones show up.</p>"
            .to_owned()
    } else {
        format!(
            "<div class=tbl><table><thead><tr><th>Id</th><th>Job</th><th>Queue</th><th>Status</th><th>Attempts</th><th>Runs at (UTC)</th><th>Last error</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"
        )
    };
    let failed = db.job_counts().await?.iter().find(|(s, _)| s == "failed").map_or(0, |(_, n)| *n);
    let bulk = if failed > 0 {
        format!(
            "<div class=actions>{}{}</div>",
            button("/kiln/jobs/retry_failed", &format!("Retry all {failed} failed"), &back, false),
            button("/kiln/jobs/discard_failed", "Discard all failed", &back, true)
        )
    } else {
        String::new()
    };
    let body = format!(
        "{queues}<h2>Jobs</h2>{}{status_tabs}{queue_tabs}{bulk}{table}",
        search_bar("/kiln/jobs", query, "Job name, payload, error or #id", &["status", "queue"])
    );
    Ok(page_with_notice("jobs", "Jobs", &body, notice))
}

async fn schedules_page(db: &Db, query: &Query, notice: Option<&str>) -> crate::Result<Response> {
    let back = link("/kiln/schedules", &[("q", param(query, "q")), ("state", param(query, "state"))]);
    let all = db.schedules().await?;
    let state = param(query, "state");
    let shown: Vec<_> = all
        .iter()
        .filter(|s| match state {
            "paused" => s.paused,
            "active" => !s.paused,
            _ => true,
        })
        .filter(|s| matches(query, &[&s.name, &s.when, &s.queue]))
        .collect();
    let rows: String = shown
        .iter()
        .map(|s| {
            let toggle = if s.paused {
                button(&format!("/kiln/schedules/{}/resume", s.name), "Resume", &back, false)
            } else {
                button(&format!("/kiln/schedules/{}/pause", s.name), "Pause", &back, false)
            };
            let run = button(&format!("/kiln/schedules/{}/run", s.name), "Run now", &back, false);
            let next = if s.paused { "<span class=\"pill failed\">paused</span>".to_owned() } else { when(s.next_run_at.as_jiff().as_second()) };
            format!(
                "<tr><td><b>{}</b></td><td><code>{}</code></td><td><code>{}</code></td><td>{next}</td><td class=muted>{}</td><td><div class=actions>{toggle}{run}</div></td></tr>",
                esc(&s.name),
                esc(&s.when),
                esc(&s.queue),
                s.last_run_at.map(|t| when(t.as_jiff().as_second())).unwrap_or_else(|| "never".into()),
            )
        })
        .collect();
    let body = if all.is_empty() {
        "<p class=empty>No schedules yet. Declare them in <code>src/schedule.rs</code> and register with <code>app.schedule(schedule::schedule)</code>; workers sync them when they start.</p>".to_owned()
    } else {
        format!(
            "{}{}{}<div class=tbl><table><thead><tr><th>Job</th><th>When</th><th>Queue</th><th>Next run (UTC)</th><th>Last run</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>\
             <p class=muted>Resuming skips the runs missed while paused. Run now enqueues the job without moving the next run.</p>",
            search_bar("/kiln/schedules", query, "Job, timing or queue", &["state"]),
            tabs(
                "/kiln/schedules",
                query,
                "state",
                &[("all", "All"), ("active", "Active"), ("paused", "Paused")],
                &["q"]
            ),
            showing(shown.len(), all.len()),
        )
    };
    Ok(page_with_notice("schedules", "Schedules", &body, notice))
}

fn progress(b: &crate::batch::BatchStatus) -> String {
    let pct = |n: i64| if b.total == 0 { 0.0 } else { n as f64 * 100.0 / b.total as f64 };
    format!(
        "<div class=bar title=\"{} succeeded, {} failed, {} cancelled, {} pending\"><div class=s style=\"width:{:.1}%\"></div><div class=f style=\"width:{:.1}%\"></div><div class=c style=\"width:{:.1}%\"></div></div>\
         <small class=muted>{}/{} done{}</small>",
        b.succeeded,
        b.failed,
        b.cancelled,
        b.pending,
        pct(b.succeeded),
        pct(b.failed),
        pct(b.cancelled),
        b.succeeded + b.failed + b.cancelled,
        b.total,
        if b.failed > 0 { format!(" · {} failed", b.failed) } else { String::new() },
    )
}

fn batch_status_pill(b: &crate::batch::BatchStatus) -> String {
    let class = match b.status.as_str() {
        "running" => "running",
        "cancelled" => "failed",
        _ if b.failed > 0 => "failed",
        _ => "queued",
    };
    format!("<span class=\"pill {class}\">{}</span>", b.status)
}

fn hooks_list(b: &crate::batch::BatchStatus) -> String {
    b.hooks
        .iter()
        .map(|(hook, job, fired)| {
            format!("<div><code>on_{hook}</code> {} {}</div>", esc(job), if *fired { "✓" } else { "" })
        })
        .collect()
}

async fn batches_page(db: &Db, query: &Query, notice: Option<&str>) -> crate::Result<Response> {
    let back = link("/kiln/batches", &[("q", param(query, "q")), ("status", param(query, "status"))]);
    let all = db.batches(1000).await?;
    let status = param(query, "status");
    let shown: Vec<_> = all
        .iter()
        .filter(|b| match status {
            "" | "all" => true,
            "failures" => b.failed > 0,
            other => b.status == other,
        })
        .filter(|b| {
            let hook_jobs: Vec<&str> = b.hooks.iter().map(|(_, job, _)| job.as_str()).collect();
            let id = format!("#{}", b.id);
            let mut fields = vec![b.description.as_str(), id.as_str()];
            fields.extend(hook_jobs);
            matches(query, &fields)
        })
        .collect();
    let rows: String = shown
        .iter()
        .map(|b| {
            let cancel = if b.status == "running" { button(&format!("/kiln/batches/{}/cancel", b.id), "Cancel", &back, true) } else { String::new() };
            format!(
                "<tr><td><a href=\"/kiln/batches/{id}\">#{id}</a></td><td><b>{}</b></td><td>{}</td><td>{}</td><td>{}</td><td class=muted>{}</td><td>{cancel}</td></tr>",
                esc(&b.description),
                batch_status_pill(b),
                progress(b),
                hooks_list(b),
                when(b.created_at.as_jiff().as_second()),
                id = b.id,
            )
        })
        .collect();
    let body = if all.is_empty() {
        "<p class=empty>No batches. Create one with <code>Batch::new(\"..\").jobs(..).on_success(..).enqueue(&amp;tx)</code>.</p>".to_owned()
    } else {
        format!(
            "{}{}{}<div class=tbl><table><thead><tr><th>Id</th><th>Batch</th><th>Status</th><th>Progress</th><th>Hooks</th><th>Created (UTC)</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>",
            search_bar("/kiln/batches", query, "Description, hook job or #id", &["status"]),
            tabs(
                "/kiln/batches",
                query,
                "status",
                &[
                    ("all", "All"),
                    ("running", "Running"),
                    ("finished", "Finished"),
                    ("cancelled", "Cancelled"),
                    ("failures", "With failures")
                ],
                &["q"]
            ),
            showing(shown.len(), all.len()),
        )
    };
    Ok(page_with_notice("batches", "Batches", &body, notice))
}

async fn batch_page(db: &Db, id: i64, notice: Option<&str>) -> crate::Result<Response> {
    let Some(b) = db.batch(crate::batch::BatchId(id)).await? else {
        return Ok((StatusCode::NOT_FOUND, "no such batch").into_response());
    };
    let back = format!("/kiln/batches/{id}");
    let jobs = db
        .fetch_all(
            "SELECT \"id\", \"name\", \"status\", \"attempts\", \"max_attempts\", COALESCE(\"last_error\", ''), \"batch_hook\" FROM \"kiln_jobs\" WHERE \"batch_id\" = ? ORDER BY \"id\"",
            vec![crate::Value::Integer(id)],
        )
        .await?;
    let rows: String = jobs
        .iter()
        .map(|r| {
            let (job, status): (i64, String) = (r.get(0), r.get(2));
            let action = match status.as_str() {
                "failed" => button(&format!("/kiln/jobs/{job}/retry"), "Retry", &back, false),
                "queued" => button(&format!("/kiln/jobs/{job}/cancel"), "Cancel", &back, true),
                _ => String::new(),
            };
            format!(
                "<tr><td>#{job}</td><td>{}{}</td><td><span class=\"pill {status}\">{status}</span></td><td>{}/{}</td><td><pre>{}</pre></td><td>{action}</td></tr>",
                esc(&r.get::<String, _>(1)),
                if r.get::<i64, _>(6) != 0 { " <span class=muted>(hook)</span>" } else { "" },
                r.get::<i64, _>(3),
                r.get::<i64, _>(4),
                esc(&r.get::<String, _>(5)),
            )
        })
        .collect();
    let cancel = if b.status == "running" {
        button(&format!("/kiln/batches/{id}/cancel"), "Cancel batch", &back, true)
    } else {
        String::new()
    };
    let body = format!(
        "<p><a href=\"/kiln/batches\">← All batches</a></p><h2>{} {}</h2><p>{}</p><dl><dt>Total</dt><dd>{}</dd><dt>Succeeded</dt><dd>{}</dd><dt>Failed</dt><dd>{}</dd><dt>Cancelled</dt><dd>{}</dd><dt>Pending</dt><dd>{}</dd><dt>Hooks</dt><dd>{}</dd></dl><p>{cancel}</p>\
         <h2>Unfinished jobs</h2><p class=muted>Jobs that succeeded are deleted; these are waiting, running or failed.</p>{}",
        esc(&b.description),
        batch_status_pill(&b),
        progress(&b),
        b.total,
        b.succeeded,
        b.failed,
        b.cancelled,
        b.pending,
        hooks_list(&b),
        if rows.is_empty() {
            "<p class=empty>None.</p>".to_owned()
        } else {
            format!(
                "<div class=tbl><table><thead><tr><th>Id</th><th>Job</th><th>Status</th><th>Attempts</th><th>Last error</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"
            )
        },
    );
    Ok(page_with_notice("batches", &format!("Batch #{id}"), &body, notice))
}

/// Handles a control form's POST. Refuses cross-site requests and posts without the token.
pub(crate) async fn act(ctx: &crate::Ctx, viewer: Viewer, request: Request) -> Response {
    VIEWER.scope(viewer, act_on(&ctx.db, request)).await
}

async fn act_on(db: &Db, request: Request) -> Response {
    if request.method() != Method::POST {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let headers = request.headers();
    let cross_site =
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|v| v != "same-origin" && v != "none");
    let origin_mismatch = match (
        headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
    ) {
        (Some(origin), Some(host)) => origin.split("://").nth(1) != Some(host),
        _ => false,
    };
    if cross_site || origin_mismatch {
        return (StatusCode::FORBIDDEN, "cross-site requests can't use /kiln controls").into_response();
    }
    let path = request.uri().path().to_owned();
    let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::BAD_REQUEST, "unreadable form").into_response(),
    };
    let form: HashMap<String, String> = serde_urlencoded::from_bytes(&bytes).unwrap_or_default();
    if form.get("_kiln").map(String::as_str) != Some(token().as_str()) || token().is_empty() {
        return (StatusCode::FORBIDDEN, "missing or stale /kiln token; reload the page").into_response();
    }
    let back = form.get("back").filter(|b| b.starts_with("/kiln")).cloned().unwrap_or_else(|| "/kiln/dashboard".into());
    let parts: Vec<&str> = path.trim_start_matches("/kiln/").split('/').collect();
    let result: crate::Result<String> = async {
        Ok(match parts[..] {
            ["jobs", "retry_failed"] => format!("Queued {} failed job(s) again.", db.retry_jobs(None).await?),
            ["jobs", "discard_failed"] => format!("Discarded {} failed job(s).", db.discard_failed_jobs().await?),
            ["jobs", id, verb] => {
                let id: i64 = id.parse().map_err(|_| crate::Error::BadRequest("bad job id".into()))?;
                let done = match verb {
                    "retry" => db.retry_jobs(Some(id)).await? > 0,
                    "cancel" => db.cancel_job(id).await?,
                    "run" => db.run_job_now(id).await?,
                    _ => return Err(crate::Error::NotFound),
                };
                let verb = match verb {
                    "retry" => "Retried",
                    "cancel" => "Cancelled",
                    _ => "Started",
                };
                if done { format!("{verb} job #{id}.") } else { format!("Job #{id} had already moved on.") }
            }
            ["queues", queue, "pause"] => {
                db.pause_queue(queue).await?;
                format!("Paused `{queue}`: running jobs finish, new ones wait.")
            }
            ["queues", queue, "resume"] => {
                db.resume_queue(queue).await?;
                format!("Resumed `{queue}`.")
            }
            ["schedules", name, "pause"] => {
                db.set_schedule_paused(name, true).await?;
                format!("Paused {name}.")
            }
            ["schedules", name, "resume"] => {
                db.set_schedule_paused(name, false).await?;
                format!("Resumed {name}.")
            }
            ["schedules", name, "run"] => match db.run_schedule_now(name).await? {
                Some(job) => format!("Enqueued {name} as job #{job}."),
                None => format!("No schedule named {name}."),
            },
            ["batches", id, "cancel"] => {
                let id: i64 = id.parse().map_err(|_| crate::Error::BadRequest("bad batch id".into()))?;
                if crate::batch::BatchId(id).cancel(db).await? {
                    format!("Cancelled batch #{id}.")
                } else {
                    format!("Batch #{id} had already ended.")
                }
            }
            _ => return Err(crate::Error::NotFound),
        })
    }
    .await;
    if crate::config::is_production() {
        let who = VIEWER.try_with(|v| v.who.clone()).unwrap_or_default();
        tracing::warn!(target: "kiln::tools", who = %who, action = %path, outcome = ?result.as_ref().map_err(|e| e.to_string()), "/kiln control used");
    }
    match result {
        Ok(message) => {
            let separator = if back.contains('?') { '&' } else { '?' };
            let location =
                format!("{back}{separator}{}", serde_urlencoded::to_string([("done", message)]).unwrap_or_default());
            Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .expect("valid redirect")
        }
        Err(e) => e.into_response(),
    }
}

async fn broadcasts_page(db: &Db, query: &Query) -> crate::Result<Response> {
    let rows = db
        .fetch_all(
            "SELECT \"id\", \"stream\", \"payload\", \"created_at\" FROM \"kiln_stream_messages\" ORDER BY \"id\" DESC LIMIT 500",
            vec![],
        )
        .await?;
    let total = rows.len();
    let shown: Vec<_> =
        rows.iter().filter(|r| matches(query, &[r.get::<&str, _>(1), r.get::<&str, _>(2)])).take(100).collect();
    let body_rows: String = shown
        .iter()
        .map(|r| {
            format!(
                "<tr><td>#{}</td><td><code>{}</code></td><td class=muted>{}</td><td><pre>{}</pre></td></tr>",
                r.get::<i64, _>(0),
                esc(&r.get::<String, _>(1)),
                when(r.get(3)),
                esc(&r.get::<String, _>(2)),
            )
        })
        .collect();
    let body = if total == 0 {
        "<p class=empty>Nothing broadcast in the last 15 minutes.</p>".to_owned()
    } else {
        format!(
            "{}<p class=muted>Newest first, from the stream log (kept 15 minutes for reconnecting pages).</p>{}<div class=tbl><table><thead><tr><th>Id</th><th>Stream</th><th>Sent (UTC)</th><th>Turbo Stream</th></tr></thead><tbody>{body_rows}</tbody></table></div>",
            search_bar("/kiln/broadcasts", query, "Stream name or content", &[]),
            showing(shown.len(), total),
        )
    };
    Ok(page("broadcasts", "Broadcasts", &body))
}

async fn sign_ins_page(db: &Db, query: &Query) -> crate::Result<Response> {
    let rows = db
        .fetch_all(
            "SELECT \"id\", \"record_table\", \"record_id\", \"user_agent\", \"ip\", \"created_at\", \"last_seen_at\" \
             FROM \"kiln_sessions\" ORDER BY \"last_seen_at\" DESC LIMIT 1000",
            vec![],
        )
        .await?;
    let total = rows.len();
    let shown: Vec<_> = rows
        .iter()
        .filter(|r| {
            let account = format!("{} #{}", r.get::<&str, _>(1), r.get::<i64, _>(2));
            let device = crate::auth::device(r.get::<&str, _>(3));
            matches(query, &[&account, &device, r.get::<&str, _>(3), r.get::<&str, _>(4)])
        })
        .collect();
    let body_rows: String = shown
        .iter()
        .map(|r| {
            format!(
                "<tr><td>#{}</td><td><code>{}</code> #{}</td><td>{}<div class=muted>{}</div></td><td>{}</td><td class=muted>{}</td><td class=muted>{}</td></tr>",
                r.get::<i64, _>(0),
                esc(&r.get::<String, _>(1)),
                r.get::<i64, _>(2),
                esc(&crate::auth::device(&r.get::<String, _>(3))),
                esc(&r.get::<String, _>(3)),
                esc(&r.get::<String, _>(4)),
                when(r.get(5)),
                when(r.get(6)),
            )
        })
        .collect();
    let body = if total == 0 {
        "<p class=empty>Nobody is signed in.</p>".to_owned()
    } else {
        format!(
            "{}{}<div class=tbl><table><thead><tr><th>Id</th><th>Account</th><th>Device</th><th>IP</th><th>Signed in (UTC)</th><th>Last active</th></tr></thead><tbody>{body_rows}</tbody></table></div>",
            search_bar("/kiln/sign_ins", query, "Account (users #3), device or IP", &[]),
            showing(shown.len(), total),
        )
    };
    Ok(page("sign_ins", "Sign-ins", &body))
}

async fn notifications_page(db: &Db, query: &Query) -> crate::Result<Response> {
    let state = param(query, "state");
    let kind = param(query, "kind");
    let rows = db
        .fetch_all(
            "SELECT \"id\", \"recipient_table\", \"recipient_id\", \"kind\", \"title\", COALESCE(\"body\", ''), COALESCE(\"url\", ''), \
             \"read_at\" IS NOT NULL, \"created_at\" FROM \"kiln_notifications\" ORDER BY \"id\" DESC LIMIT 1000",
            vec![],
        )
        .await?;
    let total = rows.len();
    let shown: Vec<_> = rows
        .iter()
        .filter(|r| match state {
            "unread" => r.get::<i64, _>(7) == 0,
            "read" => r.get::<i64, _>(7) != 0,
            _ => true,
        })
        .filter(|r| kind.is_empty() || kind == "all" || r.get::<&str, _>(3) == kind)
        .filter(|r| {
            let to = format!("{} #{}", r.get::<&str, _>(1), r.get::<i64, _>(2));
            matches(query, &[&to, r.get::<&str, _>(3), r.get::<&str, _>(4), r.get::<&str, _>(5)])
        })
        .take(200)
        .collect();
    let body_rows: String = shown
        .iter()
        .map(|r| {
            let read = r.get::<i64, _>(7) != 0;
            format!(
                "<tr><td>#{}</td><td><code>{}</code> #{}</td><td>{}</td><td><b>{}</b><div class=muted>{}</div></td><td>{}</td><td class=muted>{}</td></tr>",
                r.get::<i64, _>(0),
                esc(r.get::<&str, _>(1)),
                r.get::<i64, _>(2),
                esc(r.get::<&str, _>(3)),
                esc(r.get::<&str, _>(4)),
                esc(r.get::<&str, _>(5)),
                if read { "<span class=muted>read</span>" } else { "<span class=\"pill running\">unread</span>" },
                when(r.get::<i64, _>(8) / 1_000_000),
            )
        })
        .collect();
    let kinds = crate::notify::kinds();
    let kind_options: Vec<(&str, &str)> =
        std::iter::once(("all", "Any type")).chain(kinds.iter().map(|k| (*k, *k))).collect();
    let body = if total == 0 {
        "<p class=empty>No notifications yet. Send one with <code>MyNotification { .. }.notify(&amp;tx, &amp;user)</code>.</p>".to_owned()
    } else {
        format!(
            "{}{}{}{}<div class=tbl><table><thead><tr><th>Id</th><th>To</th><th>Type</th><th>Notification</th><th></th><th>Sent (UTC)</th></tr></thead><tbody>{body_rows}</tbody></table></div>",
            search_bar("/kiln/notifications", query, "Recipient (users #3), type or text", &["state", "kind"]),
            tabs(
                "/kiln/notifications",
                query,
                "state",
                &[("all", "All"), ("unread", "Unread"), ("read", "Read")],
                &["q", "kind"]
            ),
            if kinds.len() > 1 {
                tabs("/kiln/notifications", query, "kind", &kind_options, &["q", "state"])
            } else {
                String::new()
            },
            showing(shown.len(), total),
        )
    };
    Ok(page("notifications", "Notifications", &body))
}

/// What a page shows, cheaply: when it changes, the page refreshes.
fn signature_sql(page: &str) -> &'static str {
    const JOBS: &str = "(SELECT COUNT(*) || ':' || COALESCE(MAX(\"id\"), 0) || ':' || COALESCE(SUM(\"attempts\"), 0) || ':' || \
        COALESCE(SUM(\"status\" = 'running'), 0) || ':' || COALESCE(SUM(\"cancel_requested\"), 0) || ':' || COALESCE(SUM(\"status\" = 'failed'), 0) || ':' || \
        COALESCE(SUM(\"run_at\"), 0) FROM \"kiln_jobs\") || '|' || (SELECT COUNT(*) FROM \"kiln_queue_pauses\")";
    const BATCHES: &str = "(SELECT COUNT(*) || ':' || COALESCE(SUM(\"pending\"), 0) || ':' || COALESCE(SUM(\"succeeded\"), 0) || ':' || \
        COALESCE(SUM(\"fired_success\" + \"fired_failure\" + \"fired_complete\" + \"fired_cancel\"), 0) || ':' || COALESCE(SUM(\"status\" = 'cancelled'), 0) FROM \"kiln_batches\")";
    const SCHEDULES: &str = "(SELECT COUNT(*) || ':' || COALESCE(SUM(\"last_run_at\"), 0) || ':' || COALESCE(SUM(\"paused\"), 0) || ':' || COALESCE(SUM(\"next_run_at\"), 0) FROM \"kiln_schedules\")";
    const MAIL: &str = "(SELECT COALESCE(MAX(\"id\"), 0) FROM \"kiln_mail\")";
    const BROADCASTS: &str = "(SELECT COALESCE(MAX(\"id\"), 0) FROM \"kiln_stream_messages\")";
    const SIGN_INS: &str = "(SELECT COUNT(*) || ':' || COALESCE(MAX(\"id\"), 0) FROM \"kiln_sessions\")";
    const NOTIFICATIONS: &str =
        "(SELECT COUNT(*) || ':' || COALESCE(SUM(\"read_at\" IS NULL), 0) FROM \"kiln_notifications\")";
    match page {
        "jobs" => concat_sql(&[JOBS]),
        "batches" => concat_sql(&[BATCHES, JOBS]),
        "schedules" => concat_sql(&[SCHEDULES]),
        "mail" => concat_sql(&[MAIL]),
        "broadcasts" => concat_sql(&[BROADCASTS]),
        "sign_ins" => concat_sql(&[SIGN_INS]),
        "notifications" => concat_sql(&[NOTIFICATIONS]),
        "routes" | "records" => "SELECT ''",
        _ => concat_sql(&[JOBS, BATCHES, SCHEDULES, MAIL, BROADCASTS, SIGN_INS, NOTIFICATIONS]),
    }
}

/// `SELECT a || '|' || b ...`, built once per page kind.
fn concat_sql(parts: &[&str]) -> &'static str {
    static CACHE: std::sync::Mutex<Vec<(String, &'static str)>> = std::sync::Mutex::new(Vec::new());
    let key = parts.join("#");
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, sql)) = cache.iter().find(|(k, _)| *k == key) {
        return sql;
    }
    let sql: &'static str = Box::leak(format!("SELECT {}", parts.join(" || '|' || ")).into_boxed_str());
    cache.push((key, sql));
    sql
}

/// `/kiln/live?page=jobs`: Server-Sent Events that tell the page to refresh (Turbo morphs
/// it in place) whenever what it shows changes. Checked once a second; writes nothing.
fn live(db: Db, page: String) -> Response {
    struct State {
        db: Db,
        sql: &'static str,
        last: Option<String>,
        idle: u32,
    }
    async fn next(
        mut state: State,
    ) -> Option<(std::result::Result<axum::body::Bytes, std::convert::Infallible>, State)> {
        loop {
            tokio::select! {
                () = crate::jobs::shutdown_requested() => return None,
                () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
            let now: Option<String> =
                state.db.fetch_all(state.sql, vec![]).await.ok().and_then(|rows| rows.first().map(|r| r.get(0)));
            let changed = match (&state.last, &now) {
                (Some(before), Some(now)) => before != now,
                _ => false,
            };
            if now.is_some() {
                state.last = now;
            }
            if changed {
                state.idle = 0;
                let chunk = "data: <turbo-stream action=\"refresh\"></turbo-stream>\n\n";
                return Some((Ok(axum::body::Bytes::from_static(chunk.as_bytes())), state));
            }
            state.idle += 1;
            if state.idle >= 15 {
                state.idle = 0;
                return Some((Ok(axum::body::Bytes::from_static(b": keepalive\n\n")), state));
            }
        }
    }
    let state = State { db, sql: signature_sql(&page), last: None, idle: 0 };
    let first = futures_util::stream::once(async {
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"retry: 2000\n\n"))
    });
    let body = Body::from_stream(futures_util::StreamExt::chain(first, futures_util::stream::unfold(state, next)));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(body)
        .expect("valid stream response")
}
