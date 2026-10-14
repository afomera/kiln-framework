//! Realtime: broadcasts to pages over reliable streams.
//!
//! A broadcast is a [`TurboStream`] sent to every page subscribed to a stream name:
//!
//! ```ignore
//! ctx.db.broadcast(&format!("post:{}", post.id), TurboStream::new().append("comments", item))?;
//! ```
//!
//! ```jinja
//! {{ turbo_stream_from("post:" ~ post.id) }}
//! ```
//!
//! Every broadcast is appended to a log (`kiln_stream_messages`) before anyone hears about
//! it, so delivery doesn't depend on the connection:
//!
//! - Pages connect with Server-Sent Events. Browsers reconnect on their own and send the
//!   id of the last message they saw (`Last-Event-ID`); kiln replays everything after it.
//! - If the gap is older than the retention window, the page gets Turbo's `refresh`
//!   action instead of silently missing updates.
//! - Inside a transaction, the message is written with the transaction and listeners are
//!   woken only after it commits. A rolled-back write never broadcasts.
//! - Listeners also poll the log, so broadcasts written by another process (a background
//!   worker) arrive within a second.

use std::{
    convert::Infallible,
    sync::OnceLock,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sqlx::Row;
use tokio::sync::watch;

use crate::{Db, Error, Result, SecretKey, TurboStream, Value};

/// How long the log keeps messages for reconnecting clients.
const RETENTION: Duration = Duration::from_secs(15 * 60);
/// Listeners re-check the log at least this often (for other processes' broadcasts).
const POLL: Duration = Duration::from_secs(1);
/// Comment lines keep idle connections open through proxies.
const KEEPALIVE: Duration = Duration::from_secs(15);

const STREAMS_PATH: &str = "/kiln/streams/";

/// Wakes this process's listeners when a broadcast commits.
fn hub() -> &'static watch::Sender<u64> {
    static HUB: OnceLock<watch::Sender<u64>> = OnceLock::new();
    HUB.get_or_init(|| watch::channel(0).0)
}

fn wake_listeners() {
    hub().send_modify(|n| *n = n.wrapping_add(1));
}

fn now_seconds() -> i64 {
    jiff::Timestamp::now().as_second()
}

impl Db {
    /// Kiln's own tables, created when the database connects (never inside an app
    /// transaction, where a rollback would undo them).
    pub(crate) async fn with_kiln_tables(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_stream_messages\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \
             \"stream\" TEXT NOT NULL, \"payload\" TEXT NOT NULL, \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS \"kiln_stream_messages_stream\" ON \"kiln_stream_messages\" (\"stream\", \"id\")",
        )
        .await?;
        self.with_job_table().await?.with_blob_table().await?.with_cache_table().await?.with_webhook_table().await
    }

    /// Sends `turbo` to every page subscribed to `stream`. The stream's views render now,
    /// without a request, so they can't use `current_user`: broadcasts go to everyone.
    pub async fn broadcast(&self, stream: &str, turbo: TurboStream) -> Result<()> {
        let payload = turbo.render()?;
        // The stream log in its own database can't join the caller's transaction: the
        // message is written once it commits (and never if it rolls back).
        if self.hands_over(crate::db::Role::Streams) {
            let (streams, stream) = (self.role(crate::db::Role::Streams), stream.to_owned());
            self.after_commit_async(move || async move {
                if let Err(e) = streams.write_broadcast(&stream, payload).await {
                    tracing::error!(target: "kiln::realtime", stream, error = %e, "broadcasting after the transaction committed failed");
                }
            })
            .await;
            return Ok(());
        }
        self.role(crate::db::Role::Streams).write_broadcast(stream, payload).await
    }

    async fn write_broadcast(&self, stream: &str, payload: String) -> Result<()> {
        let rows = self
            .fetch_all(
                "INSERT INTO \"kiln_stream_messages\" (\"stream\", \"payload\", \"created_at\") VALUES (?, ?, ?) RETURNING \"id\"",
                vec![Value::Text(stream.to_owned()), Value::Text(payload), Value::Integer(now_seconds())],
            )
            .await?;
        let id: i64 = rows.first().map(|r| r.get(0)).unwrap_or_default();
        if id % 64 == 0 {
            let cutoff = now_seconds() - RETENTION.as_secs() as i64;
            self.fetch_all(
                "DELETE FROM \"kiln_stream_messages\" WHERE \"created_at\" < ? RETURNING \"id\"",
                vec![Value::Integer(cutoff)],
            )
            .await?;
        }
        self.after_commit(wake_listeners);
        Ok(())
    }

    /// Messages on `stream` after `after`, oldest first: `(id, payload)`.
    pub async fn stream_messages(&self, stream: &str, after: i64) -> Result<Vec<(i64, String)>> {
        let rows = self
            .role(crate::db::Role::Streams)
            .fetch_all(
                "SELECT \"id\", \"payload\" FROM \"kiln_stream_messages\" WHERE \"stream\" = ? AND \"id\" > ? ORDER BY \"id\" LIMIT 500",
                vec![Value::Text(stream.to_owned()), Value::Integer(after)],
            )
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// `(oldest, newest)` ids still in the log.
    async fn stream_log_bounds(&self) -> Result<(Option<i64>, i64)> {
        let rows = self
            .role(crate::db::Role::Streams)
            .fetch_all("SELECT MIN(\"id\"), MAX(\"id\") FROM \"kiln_stream_messages\"", vec![])
            .await?;
        let row = rows.first().ok_or(Error::NotFound)?;
        Ok((row.get(0), row.get::<Option<i64>, _>(1).unwrap_or(0)))
    }
}

/// `<turbo-stream-source>` for a stream, with its name signed so a page can only listen to
/// streams it was rendered with.
pub(crate) fn source_tag(key: &SecretKey, stream: &str) -> String {
    format!("<turbo-stream-source src=\"{STREAMS_PATH}{}\"></turbo-stream-source>", key.sign_str(stream))
}

/// Serves `/kiln/streams/<signed name>` as Server-Sent Events.
pub(crate) async fn serve(db: Db, key: &SecretKey, path: &str, last_event_id: Option<&str>) -> Option<Response> {
    let token = path.strip_prefix(STREAMS_PATH)?;
    let Some(stream) = key.verify_str(token) else {
        return Some((StatusCode::NOT_FOUND, "unknown stream").into_response());
    };
    let (oldest, newest) = match db.stream_log_bounds().await {
        Ok(bounds) => bounds,
        Err(e) => return Some(e.into_response()),
    };
    let resume = last_event_id.and_then(|id| id.trim().parse::<i64>().ok());
    // A gap the log no longer covers: the page must refetch.
    let missed = matches!((resume, oldest), (Some(last), Some(oldest)) if last + 1 < oldest);
    let start = match resume {
        Some(last) if !missed => last,
        _ => newest,
    };

    let mut first = String::from("retry: 2000\n\n");
    if missed {
        first.push_str(&event(newest, "<turbo-stream action=\"refresh\"></turbo-stream>"));
    }
    let state = Listener { db, stream, last: start, wake: hub().subscribe(), pending: Some(first) };
    let body = Body::from_stream(futures_util::stream::unfold(state, next_chunk));
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Some(response)
}

struct Listener {
    db: Db,
    stream: String,
    last: i64,
    wake: watch::Receiver<u64>,
    pending: Option<String>,
}

/// One SSE message: `id:` lets the browser resume from here after a reconnect.
fn event(id: i64, payload: &str) -> String {
    let mut text = format!("id: {id}\n");
    for line in payload.lines() {
        text.push_str("data: ");
        text.push_str(line);
        text.push('\n');
    }
    text.push('\n');
    text
}

async fn next_chunk(mut state: Listener) -> Option<(std::result::Result<Bytes, Infallible>, Listener)> {
    if let Some(first) = state.pending.take() {
        return Some((Ok(Bytes::from(first)), state));
    }
    let idle_since = Instant::now();
    loop {
        let messages = match state.db.stream_messages(&state.stream, state.last).await {
            Ok(messages) => messages,
            Err(e) => {
                tracing::error!(target: "kiln::realtime", error = %e, "reading the stream log failed");
                return None;
            }
        };
        if let Some((last, _)) = messages.last() {
            state.last = *last;
            let chunk: String = messages.iter().map(|(id, payload)| event(*id, payload)).collect();
            return Some((Ok(Bytes::from(chunk)), state));
        }
        if idle_since.elapsed() >= KEEPALIVE {
            return Some((Ok(Bytes::from_static(b": keepalive\n\n")), state));
        }
        tokio::select! {
            changed = state.wake.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            // A shutting-down server ends its streams; browsers reconnect elsewhere.
            () = crate::jobs::shutdown_requested() => return None,
            () = tokio::time::sleep(POLL) => {}
        }
    }
}
