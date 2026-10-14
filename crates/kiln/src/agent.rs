//! Development commands for agents, behind `kiln mcp`: a request made as any user with the
//! SQL it ran and the jobs and mail it caused, read-only SQL, the outbox and job state. All
//! JSON. Debug builds only, and never against production.

use axum::{body::Body, http::Method};
use serde_json::{Value, json};
use sqlx::{Column, Row, TypeInfo};

use crate::{
    App, Db, Error, Result,
    db::Role,
    testing::{TestClient, Upload},
};

pub(crate) const USAGE: &str = "usage: request <METHOD> <path> [--as User:1] [--form key=value].. [--file field=path].. \
                     [--json '{..}'] [--accept html|json|turbo] [--follow] [--perform-jobs] [--body text|html|none]";

/// A file from disk for `--file`; its type comes from the extension (Kiln checks the bytes).
fn upload(path: &str) -> Result<Upload> {
    let bytes = std::fs::read(path).map_err(|e| Error::BadRequest(format!("--file {path}: {e}")))?;
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let content_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    let filename = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    Ok(Upload::new(filename, content_type, bytes))
}

/// Bodies longer than this are cut (with a note), so one page can't flood an agent.
const MAX_BODY: usize = 200_000;

/// `request <METHOD> <path> [--as <Record>:<id> | <id> | <email>] [--form key=value]..
/// [--file field=path].. [--json <body>] [--accept html|json|turbo] [--follow]
/// [--perform-jobs] [--body text|html|none]`. HTML bodies come back as readable text unless
/// `--body html`.
pub(crate) async fn request(app: &App, args: &[&str]) -> Result<Value> {
    let bad = |m: String| Error::BadRequest(m);
    let (method, path) = match args {
        [method, path, ..] => (*method, *path),
        _ => {
            return Err(bad(USAGE.into()));
        }
    };
    let method: Method =
        method.to_ascii_uppercase().parse().map_err(|_| bad(format!("`{method}` isn't an HTTP method")))?;
    let mut as_user = None;
    let mut form: Vec<(String, String)> = vec![];
    let mut files: Vec<(String, Upload)> = vec![];
    let mut json_body = None;
    let mut accept = "text/html,application/xhtml+xml,*/*;q=0.8";
    let mut follow = false;
    let mut perform_jobs = false;
    let mut body_mode = "text";
    let mut rest = args[2..].iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().copied().ok_or_else(|| bad(format!("{arg} needs a value")));
        match *arg {
            "--as" => as_user = Some(value()?.to_owned()),
            "--form" => {
                let pair = value()?;
                let (k, v) =
                    pair.split_once('=').ok_or_else(|| bad(format!("--form takes key=value (got `{pair}`)")))?;
                form.push((k.to_owned(), v.to_owned()));
            }
            "--file" => {
                let pair = value()?;
                let (k, path) =
                    pair.split_once('=').ok_or_else(|| bad(format!("--file takes field=path (got `{pair}`)")))?;
                files.push((k.to_owned(), upload(path)?));
            }
            "--json" => json_body = Some(value()?.to_owned()),
            "--accept" => {
                accept = match value()? {
                    "html" => "text/html,application/xhtml+xml,*/*;q=0.8",
                    "json" => "application/json",
                    "turbo" => "text/vnd.turbo-stream.html, text/html",
                    other => return Err(bad(format!("--accept is html, json or turbo (got `{other}`)"))),
                }
            }
            "--follow" => follow = true,
            "--perform-jobs" => perform_jobs = true,
            "--body" => {
                body_mode = match value()? {
                    mode @ ("text" | "html" | "none") => mode,
                    other => return Err(bad(format!("--body is text, html or none (got `{other}`)"))),
                }
            }
            other => return Err(bad(format!("unknown option `{other}`"))),
        }
    }

    let db = &app.ctx().db;
    let client = TestClient::new(app);
    let signed_in = match &as_user {
        Some(who) => {
            let (table, id) = resolve_user(db, who).await?;
            client.sign_in_row(table, id);
            client.store_pending_sign_in().await;
            Some((table, id))
        }
        None => None,
    };
    let jobs_before = max_id(&db.role(Role::Queue), "kiln_jobs").await;
    let mail_before = max_id(db, "kiln_mail").await;

    let started = std::time::Instant::now();
    let (response, sql) = crate::db::capturing_sql(async {
        let body = if let Some(json) = &json_body {
            Some(("application/json".to_owned(), Body::from(json.clone())))
        } else if !files.is_empty() {
            let fields: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let files: Vec<(&str, Upload)> = files.iter().map(|(k, f)| (k.as_str(), f.clone())).collect();
            let (content_type, body) = client.multipart_body(&fields, &files);
            Some((content_type, Body::from(body)))
        } else if !form.is_empty() || method != Method::GET {
            let fields: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let (content_type, body) = client.form_body(&fields, true);
            Some((content_type.to_owned(), Body::from(body)))
        } else {
            None
        };
        let token = client.csrf_token();
        let headers: Vec<(&str, &str)> = if json_body.is_some() { vec![("x-csrf-token", &token)] } else { vec![] };
        let (content_type, body) = body.unzip();
        let body = content_type.as_deref().zip(body);
        let mut response = client.send_with(method.clone(), path, Some(accept), body, &headers).await;
        let mut redirects = vec![];
        while follow && response.status.is_redirection() && redirects.len() < 5 {
            let Some(location) = response.headers.get("location").and_then(|l| l.to_str().ok()).map(str::to_owned)
            else {
                break;
            };
            redirects
                .push(json!({ "status": response.status.as_u16(), "location": location, "flash": response.flash }));
            response = client.send_with(Method::GET, &location, Some(accept), None, &[]).await;
        }
        (response, redirects)
    })
    .await;
    let elapsed = started.elapsed();
    let (response, redirects) = response;

    if let Some((table, _)) = signed_in
        && let Some(token) = client.session_string(&format!("current.{table}"))
    {
        // Don't leave a "kiln test client" row in the person's list of sign-ins.
        let _ = crate::auth::delete_sign_in(db, table, &token).await;
    }

    let jobs = new_rows(
        &db.role(Role::Queue),
        "SELECT \"id\", \"name\", \"queue\", \"payload\", \"status\" FROM \"kiln_jobs\" WHERE \"id\" > ? ORDER BY \"id\"",
        jobs_before,
    )
    .await;
    // Mail sent by jobs (`deliver_later`, notifications) shows up only once they run.
    let jobs_performed = if perform_jobs {
        match crate::jobs::perform_all(app.ctx()).await {
            Ok(count) => json!(count),
            Err(e) => json!({ "error": e.to_string() }),
        }
    } else {
        Value::Null
    };
    let mail: Vec<Value> = db
        .outbox()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| *id > mail_before)
        .map(|(id, m)| json!({ "id": id, "to": m.to, "subject": m.subject, "text": m.text }))
        .collect();
    let content_type = response.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let mut body = match body_mode {
        "none" => String::new(),
        "html" => response.text.clone(),
        // A development error page: its Markdown report (`error_report`) says it better.
        _ if response.error_report.is_some() => String::new(),
        _ if content_type.starts_with("text/html") => html_to_text(&response.text),
        _ => response.text.clone(),
    };
    let truncated = body.len() > MAX_BODY;
    if truncated {
        let mut end = MAX_BODY;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    let headers: serde_json::Map<String, Value> = response
        .headers
        .iter()
        .filter(|(name, _)| *name != "set-cookie")
        .map(|(name, value)| (name.to_string(), Value::from(value.to_str().unwrap_or("<binary>"))))
        .collect();
    Ok(json!({
        "status": response.status.as_u16(),
        "redirects": redirects,
        "location": response.headers.get("location").and_then(|l| l.to_str().ok()),
        "flash": response.flash,
        "error_report": response.error_report,
        "content_type": content_type,
        "headers": headers,
        "body": (body_mode != "none" && !(body.is_empty() && response.error_report.is_some())).then_some(body),
        "body_truncated": truncated,
        "signed_in_as": signed_in.map(|(table, id)| json!({ "table": table, "id": id })),
        "elapsed_ms": elapsed.as_millis() as u64,
        "sql": sql,
        "jobs_enqueued": jobs,
        "jobs_performed": jobs_performed,
        "mail_sent": mail,
    }))
}

/// `User:3`, `3` (the first record that signs in), or an email address.
async fn resolve_user(db: &Db, who: &str) -> Result<(&'static str, i64)> {
    let records = crate::migrate::records();
    let (record, key) = match who.split_once(':') {
        Some((name, key)) => {
            let record = records
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name) || r.table == name)
                .ok_or_else(|| Error::BadRequest(format!("no record named `{name}`")))?;
            (*record, key)
        }
        None => {
            let record = records
                .iter()
                .find(|r| r.name == "User")
                .or_else(|| records.iter().find(|r| (r.schema)().columns.iter().any(|c| c.name == "email")))
                .ok_or_else(|| Error::BadRequest("say which record: --as User:1".into()))?;
            (*record, who)
        }
    };
    if let Ok(id) = key.parse::<i64>() {
        return Ok((record.table, id));
    }
    let rows = db
        .fetch_all(
            &format!("SELECT \"id\" FROM \"{}\" WHERE \"email\" = ? LIMIT 1", record.table),
            vec![crate::Value::Text(key.to_owned())],
        )
        .await?;
    let id: i64 = rows
        .first()
        .map(|r| r.get(0))
        .ok_or_else(|| Error::BadRequest(format!("no {} with email `{key}`", record.name)))?;
    Ok((record.table, id))
}

async fn max_id(db: &Db, table: &str) -> i64 {
    db.fetch_all(&format!("SELECT COALESCE(MAX(\"id\"), 0) FROM \"{table}\""), vec![])
        .await
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.try_get::<i64, _>(0).ok()))
        .unwrap_or(0)
}

async fn new_rows(db: &Db, sql: &str, after: i64) -> Vec<Value> {
    match db.fetch_all(sql, vec![crate::Value::Integer(after)]).await {
        Ok(rows) => rows.iter().map(row_json).map(Value::Object).collect(),
        Err(_) => vec![],
    }
}

/// `db query <sql> [--limit N] [--database queue|cache|streams]`: read-only, on one
/// connection the database itself holds to read-only.
pub(crate) async fn query(db: &Db, args: &[&str]) -> Result<Value> {
    let mut sql = None;
    let mut limit = 100usize;
    let mut db = db.clone();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--limit" => limit = rest.next().and_then(|n| n.parse().ok()).unwrap_or(limit),
            "--database" => {
                db = match rest.next().copied() {
                    Some("queue") => db.role(Role::Queue),
                    Some("cache") => db.role(Role::Cache),
                    Some("streams") => db.role(Role::Streams),
                    Some("main") | None => db,
                    Some(other) => return Err(Error::BadRequest(format!("no `{other}` database"))),
                }
            }
            text => sql = Some(text),
        }
    }
    let sql = sql.ok_or_else(|| Error::BadRequest("usage: db query \"SELECT ..\"".into()))?;
    let mut conn = db.acquire().await?;
    let sqlite = db.dialect().name() == "sqlite";
    let (on, off) = if sqlite {
        ("PRAGMA query_only = ON", "PRAGMA query_only = OFF")
    } else {
        (
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        )
    };
    sqlx::query(on).execute(&mut *conn).await?;
    let result = sqlx::query(sql).fetch_all(&mut *conn).await;
    sqlx::query(off).execute(&mut *conn).await?;
    let rows = result?;
    let total = rows.len();
    let columns: Vec<String> =
        rows.first().map(|r| r.columns().iter().map(|c| c.name().to_owned()).collect()).unwrap_or_default();
    let rows: Vec<Value> = rows.iter().take(limit).map(|r| Value::Object(row_json(r))).collect();
    Ok(json!({ "columns": columns, "rows": rows, "row_count": total, "truncated": total > limit }))
}

fn row_json(row: &crate::db::Row) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (i, column) in row.columns().iter().enumerate() {
        let value = if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
            // Kiln stores times as microseconds since 1970; show them as times.
            match v.filter(|_| column.name().ends_with("_at")).and_then(|us| jiff::Timestamp::from_microsecond(us).ok())
            {
                Some(at) if v.unwrap_or_default().abs() > 100_000_000_000_000 => json!(at.to_string()),
                _ => json!(v),
            }
        } else if let Ok(v) = row.try_get::<Option<f64>, _>(i) {
            json!(v)
        } else if let Ok(v) = row.try_get::<Option<String>, _>(i) {
            json!(v)
        } else if let Ok(v) = row.try_get::<Option<bool>, _>(i) {
            json!(v)
        } else if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(i) {
            json!(v.map(|b| format!("<{} bytes>", b.len())))
        } else {
            json!(format!("<{}>", column.type_info().name()))
        };
        out.insert(column.name().to_owned(), value);
    }
    out
}

/// `mail [--limit N]`: the newest mail in the development outbox.
pub(crate) async fn outbox(db: &Db, args: &[&str]) -> Result<Value> {
    let limit = match args {
        ["--limit", n] => n.parse().unwrap_or(10),
        _ => 10,
    };
    let mail = db.outbox().await?;
    let newest: Vec<Value> = mail
        .into_iter()
        .rev()
        .take(limit)
        .map(|(id, m)| json!({ "id": id, "to": m.to, "from": m.from, "subject": m.subject, "text": m.text, "html": m.html }))
        .collect();
    Ok(json!({ "mail": newest }))
}

/// `jobs --json`: counts, queues and failures.
pub(crate) async fn jobs(db: &Db) -> Result<Value> {
    let counts: serde_json::Map<String, Value> =
        db.job_counts().await?.into_iter().map(|(status, n)| (status, Value::from(n))).collect();
    let queues: Vec<Value> = db
        .queues()
        .await?
        .into_iter()
        .map(|(queue, paused, waiting, running, failed)| {
            json!({ "queue": queue, "paused": paused, "waiting": waiting, "running": running, "failed": failed })
        })
        .collect();
    let failed: Vec<Value> = db
        .failed_jobs()
        .await?
        .into_iter()
        .map(|(id, name, attempts, error)| json!({ "id": id, "name": name, "attempts": attempts, "error": error }))
        .collect();
    Ok(json!({ "counts": counts, "queues": queues, "failed": failed }))
}

/// An HTML page as text an agent can read cheaply: headings, paragraphs, list items, links
/// as `[text](href)`, forms and their fields, buttons; scripts, styles and SVG dropped.
pub(crate) fn html_to_text(html: &str) -> String {
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

    #[tokio::test]
    async fn requests_report_status_body_and_sql() {
        let db = Db::memory().await.unwrap();
        db.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)").await.unwrap();
        let app = App::new(db.clone());
        let out = request(&app, &["GET", "/up"]).await.unwrap();
        assert_eq!(out["status"], 200);
        assert_eq!(out["jobs_enqueued"], json!([]));
        let missing = request(&app, &["GET", "/nope"]).await.unwrap();
        assert_eq!(missing["status"], 404);
        assert!(missing["error_report"].as_str().unwrap().contains("(K1001)"), "{missing}");

        let (_, sql) =
            crate::db::capturing_sql(async { db.fetch_all("SELECT COUNT(*) FROM notes", vec![]).await }).await;
        assert_eq!(sql, ["SELECT COUNT(*) FROM notes"]);
        assert!(request(&app, &["GET"]).await.is_err());
        assert!(request(&app, &["GET", "/", "--accept", "xml"]).await.is_err());
        assert!(request(&app, &["GET", "/", "--as", "Nobody:1"]).await.is_err());
    }

    #[tokio::test]
    async fn queries_are_read_only() {
        let db = Db::memory().await.unwrap();
        db.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)").await.unwrap();
        db.execute("INSERT INTO notes (body) VALUES ('a'), ('b'), ('c')").await.unwrap();
        let out = query(&db, &["SELECT id, body FROM notes ORDER BY id", "--limit", "2"]).await.unwrap();
        assert_eq!(out["columns"], json!(["id", "body"]));
        assert_eq!(out["rows"][1], json!({ "id": 2, "body": "b" }));
        assert_eq!(out["row_count"], 3);
        assert_eq!(out["truncated"], true);
        assert!(query(&db, &["DELETE FROM notes"]).await.is_err());
        // The connection goes back to the pool writable.
        db.execute("INSERT INTO notes (body) VALUES ('d')").await.unwrap();
    }

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
}
