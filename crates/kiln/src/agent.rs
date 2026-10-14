//! Development commands for agents, behind `kiln mcp`: a request made as any user with the
//! SQL it ran and the jobs and mail it caused, read-only SQL, the outbox and job state. All
//! JSON. Debug builds only, and never against production.

use axum::{body::Body, http::Method};
use serde_json::{Value, json};
use sqlx::{Column, Row, TypeInfo};

use crate::{App, Db, Error, Result, db::Role, testing::TestClient};

/// Bodies longer than this are cut (with a note), so one page can't flood an agent.
const MAX_BODY: usize = 200_000;

/// `request <METHOD> <path> [--as <Record>:<id> | <id> | <email>] [--form key=value]..
/// [--json <body>] [--accept html|json|turbo] [--follow]`.
pub(crate) async fn request(app: &App, args: &[&str]) -> Result<Value> {
    let bad = |m: String| Error::BadRequest(m);
    let (method, path) = match args {
        [method, path, ..] => (*method, *path),
        _ => {
            return Err(
                bad("usage: request <METHOD> <path> [--as User:1] [--form key=value].. [--json '{..}']".into()),
            );
        }
    };
    let method: Method =
        method.to_ascii_uppercase().parse().map_err(|_| bad(format!("`{method}` isn't an HTTP method")))?;
    let mut as_user = None;
    let mut form: Vec<(String, String)> = vec![];
    let mut json_body = None;
    let mut accept = "text/html,application/xhtml+xml,*/*;q=0.8";
    let mut follow = false;
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
            Some(("application/json", Body::from(json.clone())))
        } else if !form.is_empty() || method != Method::GET {
            let fields: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let (content_type, body) = client.form_body(&fields, true);
            Some((content_type, Body::from(body)))
        } else {
            None
        };
        let token = client.csrf_token();
        let headers: Vec<(&str, &str)> = if json_body.is_some() { vec![("x-csrf-token", &token)] } else { vec![] };
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
    let mail: Vec<Value> = db
        .outbox()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| *id > mail_before)
        .map(|(id, m)| json!({ "id": id, "to": m.to, "subject": m.subject, "text": m.text }))
        .collect();
    let content_type = response.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let mut body = response.text.clone();
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
        "body": body,
        "body_truncated": truncated,
        "signed_in_as": signed_in.map(|(table, id)| json!({ "table": table, "id": id })),
        "elapsed_ms": elapsed.as_millis() as u64,
        "sql": sql,
        "jobs_enqueued": jobs,
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
            json!(v)
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
}
