//! Background jobs, stored in the database.
//!
//! ```ignore
//! #[derive(Job, Serialize, Deserialize)]
//! #[job(queue = "mail", attempts = 5)]
//! pub struct NotifySubscribers {
//!     pub post_id: Id<Post>,
//! }
//!
//! impl Perform for NotifySubscribers {
//!     async fn perform(self, ctx: &Ctx) -> Result { .. }
//! }
//!
//! NotifySubscribers { post_id: post.id }.enqueue(&tx).await?;   // inside the write's transaction
//! ```
//!
//! Jobs live in `kiln_jobs`, so enqueueing inside a transaction is atomic with the write:
//! a rolled-back write never runs its job. Workers claim jobs one at a time, delete them on
//! success, and retry failures with exponential backoff until the job's attempts run out,
//! when it's marked `failed` (see `kiln jobs`). A job whose worker died is claimed again
//! after five minutes, so jobs should be safe to run twice.

use std::{future::Future, sync::OnceLock, time::Duration};

use serde::{Serialize, de::DeserializeOwned};
use sqlx::Row;
use tokio::sync::watch;

use crate::{
    BoxFuture, Ctx, Db, Error, Result, Timestamp, Value,
    batch::{self, BatchId, CURRENT_BATCH},
};

/// Implemented by `#[derive(Job)]`.
pub trait JobInfo: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable name stored with each job; the struct's name.
    const NAME: &'static str;
    const QUEUE: &'static str = "default";
    /// Runs before the job is marked failed (including the first).
    const ATTEMPTS: i64 = 5;
    /// `#[job(unique_for = "10m")]`: enqueueing a job equal to one queued or running that
    /// was enqueued within this many seconds returns that job instead of adding another.
    const UNIQUE_FOR: Option<i64> = None;
    /// `#[job(priority = 10)]`: due jobs with a higher priority run first. Default 0.
    const PRIORITY: i64 = 0;
    /// `#[job(timeout = "30s")]`: an attempt still running after this many seconds is
    /// stopped and counts as a failure (so it retries like any other).
    const TIMEOUT: Option<i64> = None;
}

/// What a job does. Write it for every `#[derive(Job)]` struct.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a job but doesn't say what it does",
    label = "missing `impl Perform for {Self}`",
    note = "add `impl Perform for {Self} {{ async fn perform(self, ctx: &Ctx) -> Result {{ .. }} }}`"
)]
pub trait Perform: JobInfo {
    fn perform(self, ctx: &Ctx) -> impl Future<Output = Result> + Send;
}

/// A job type linked into the binary, registered by `#[derive(Job)]`.
pub struct JobDef {
    pub name: &'static str,
    pub run: fn(Ctx, serde_json::Value) -> BoxFuture<'static, Result>,
    /// Seconds an attempt may run (`JobInfo::TIMEOUT`).
    pub timeout: Option<i64>,
}

/// Per-queue caps on running jobs, across every process: `app.queue_limit("imports", 2)`.
#[derive(Clone, Default)]
pub struct QueueLimits(pub(crate) Vec<(&'static str, i64)>);

/// Writes the app's schedules and queue limits to the database. Every process does this at
/// boot, so the dashboard and workers agree even when jobs run elsewhere.
pub(crate) async fn sync_app(ctx: &Ctx) -> Result<()> {
    if let Some(schedule) = ctx.try_get::<crate::Schedule>() {
        ctx.db.sync_schedules(schedule, Timestamp::now()).await?;
    }
    let limits = ctx.try_get::<QueueLimits>().cloned().unwrap_or_default();
    ctx.db
        .role(crate::db::Role::Queue)
        .transaction(|tx| async move {
            tx.role(crate::db::Role::Queue)
                .fetch_all("DELETE FROM \"kiln_queue_limits\" RETURNING \"queue\"", vec![])
                .await?;
            for (queue, max) in limits.0 {
                tx.role(crate::db::Role::Queue).fetch_all(
                    "INSERT INTO \"kiln_queue_limits\" (\"queue\", \"max_running\") VALUES (?, ?) RETURNING \"queue\"",
                    vec![Value::Text(queue.to_owned()), Value::Integer(max.max(1))],
                )
                .await?;
            }
            Ok(())
        })
        .await
}

/// How an attempt ended.
enum Outcome {
    Done,
    Failed(String),
    /// Cancelled while running.
    Cancelled,
    /// Stopped by a shutdown: back to the queue, the attempt not counted.
    Released,
}

inventory::collect!(JobDef);

fn job_def(name: &str) -> Option<&'static JobDef> {
    inventory::iter::<JobDef>.into_iter().find(|d| d.name == name)
}

/// Used by `#[derive(Job)]`.
#[doc(hidden)]
pub fn run_job<J: Perform>(ctx: Ctx, payload: serde_json::Value) -> BoxFuture<'static, Result> {
    Box::pin(async move {
        let job: J = serde_json::from_value(payload)
            .map_err(|e| Error::BadRequest(format!("job `{}` has an unreadable payload: {e}", J::NAME)))?;
        job.perform(&ctx).await
    })
}

/// Enqueueing, for every job.
pub trait Enqueue: JobInfo {
    /// Runs as soon as a worker is free. Inside a transaction, only if it commits.
    fn enqueue(&self, db: &Db) -> impl Future<Output = Result<i64>> + Send {
        self.enqueue_at(db, Timestamp::now())
    }

    fn enqueue_in(&self, db: &Db, delay: jiff::SignedDuration) -> impl Future<Output = Result<i64>> + Send {
        self.enqueue_at(db, Timestamp::now() + delay)
    }

    fn enqueue_at(&self, db: &Db, at: Timestamp) -> impl Future<Output = Result<i64>> + Send {
        let payload = serde_json::to_string(self);
        async move {
            let payload =
                payload.map_err(|e| Error::BadRequest(format!("job `{}` doesn't serialize: {e}", Self::NAME)))?;
            if let Some(window) = Self::UNIQUE_FOR {
                let since = micros(Timestamp::now()) - window * 1_000_000;
                let existing = db
                    .role(crate::db::Role::Queue)
                    .fetch_all(
                        "SELECT \"id\" FROM \"kiln_jobs\" WHERE \"name\" = ? AND \"payload\" = ? \
                         AND \"status\" IN ('queued', 'running') AND \"created_at\" >= ? LIMIT 1",
                        vec![Value::Text(Self::NAME.to_owned()), Value::Text(payload.clone()), Value::Integer(since)],
                    )
                    .await?;
                if let Some(row) = existing.first() {
                    return Ok(row.get(0));
                }
            }
            insert_row(db, Self::NAME, Self::QUEUE, payload, micros(at), Self::ATTEMPTS, Self::PRIORITY, None, false)
                .await
        }
    }
}

/// Stores a job row; workers are woken when it commits.
pub(crate) async fn insert_job(
    db: &Db,
    name: &str,
    queue: &str,
    payload: String,
    run_at: i64,
    max_attempts: i64,
    priority: i64,
) -> Result<i64> {
    insert_row(db, name, queue, payload, run_at, max_attempts, priority, None, false).await
}

/// Stores a job that belongs to `batch` (as one of its jobs, or as a `hook`), due now.
pub(crate) async fn insert_job_in_batch(
    db: &Db,
    spec: &crate::batch::JobSpec,
    batch: Option<BatchId>,
    hook: bool,
) -> Result<i64> {
    insert_row(
        db,
        &spec.name,
        &spec.queue,
        spec.payload.clone(),
        micros(Timestamp::now()),
        spec.max_attempts,
        spec.priority,
        batch,
        hook,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_row(
    db: &Db,
    name: &str,
    queue: &str,
    payload: String,
    run_at: i64,
    max_attempts: i64,
    priority: i64,
    batch: Option<BatchId>,
    hook: bool,
) -> Result<i64> {
    // A queue in its own database can't join the caller's transaction: the job waits as
    // `pending` (never claimed) until the transaction commits, and goes if it rolls back.
    let handing_over = db.hands_over(crate::db::Role::Queue);
    let held = handing_over || HOLD_JOBS.try_with(|hold| *hold).unwrap_or(false);
    let status = if held { "pending" } else { "queued" };
    let queue_db = db.role(crate::db::Role::Queue);
    let rows = queue_db
        .fetch_all(
            &format!(
                "INSERT INTO \"kiln_jobs\" (\"name\", \"queue\", \"payload\", \"run_at\", \"attempts\", \"max_attempts\", \"status\", \"created_at\", \"batch_id\", \"batch_hook\", \"priority\") \
                 VALUES (?, ?, ?, ?, 0, ?, '{status}', ?, ?, ?, ?) RETURNING \"id\""
            ),
            vec![
                Value::Text(name.to_owned()),
                Value::Text(queue.to_owned()),
                Value::Text(payload),
                Value::Integer(run_at),
                Value::Integer(max_attempts),
                Value::Integer(micros(Timestamp::now())),
                batch.map_or(Value::Null, |b| Value::Integer(b.0)),
                Value::Integer(hook as i64),
                Value::Integer(priority),
            ],
        )
        .await?;
    let id: i64 = rows.first().map(|r| r.get(0)).unwrap_or_default();
    if handing_over {
        let (release, discard) = (queue_db.clone(), queue_db);
        db.after_commit_async(move || async move {
            let released = release
                .fetch_all(
                    "UPDATE \"kiln_jobs\" SET \"status\" = 'queued' WHERE \"id\" = ? AND \"status\" = 'pending' RETURNING \"id\"",
                    vec![Value::Integer(id)],
                )
                .await;
            if let Err(e) = released {
                tracing::error!(target: "kiln::jobs", job = id, error = %e, "releasing a job after its transaction committed failed");
            }
            wake_workers();
        })
        .await;
        db.after_rollback(move || async move {
            let _ = discard
                .fetch_all("DELETE FROM \"kiln_jobs\" WHERE \"id\" = ? RETURNING \"id\"", vec![Value::Integer(id)])
                .await;
        });
    } else {
        db.after_commit(wake_workers);
    }
    Ok(id)
}

tokio::task_local! {
    /// Set while a batch is created for a transaction in another database: its jobs wait
    /// as `pending` until the batch is released.
    pub(crate) static HOLD_JOBS: bool;
}

/// Jobs held as `pending` whose transaction never committed (a crash in between), cleared
/// after an hour.
pub(crate) async fn clear_stale_pending(db: &Db) -> Result<u64> {
    let cutoff = micros(Timestamp::now() - jiff::SignedDuration::from_hours(1));
    let queue = db.role(crate::db::Role::Queue);
    let rows = queue
        .fetch_all(
            "DELETE FROM \"kiln_jobs\" WHERE \"status\" = 'pending' AND \"created_at\" < ? RETURNING \"id\"",
            vec![Value::Integer(cutoff)],
        )
        .await?;
    queue
        .fetch_all(
            "DELETE FROM \"kiln_batches\" WHERE \"status\" = 'pending' AND \"created_at\" < ? RETURNING \"id\"",
            vec![Value::Integer(cutoff)],
        )
        .await?;
    Ok(rows.len() as u64)
}

impl<J: JobInfo> Enqueue for J {}

fn micros(ts: Timestamp) -> i64 {
    ts.as_jiff().as_microsecond()
}

/// Wakes this process's workers when a job commits.
fn hub() -> &'static watch::Sender<u64> {
    static HUB: OnceLock<watch::Sender<u64>> = OnceLock::new();
    HUB.get_or_init(|| watch::channel(0).0)
}

pub(crate) fn wake_workers() {
    hub().send_modify(|n| *n = n.wrapping_add(1));
}

/// A claimed job.
struct Claimed {
    id: i64,
    name: String,
    payload: String,
    attempts: i64,
    max_attempts: i64,
    /// The batch it belongs to, and whether it's one of the batch's hooks (which don't
    /// count toward the batch).
    batch: Option<BatchId>,
    hook: bool,
}

impl Claimed {
    /// The batch this job counts toward, if any.
    fn counted_batch(&self) -> Option<BatchId> {
        self.batch.filter(|_| !self.hook)
    }
}

/// Jobs held this long by a worker are assumed abandoned and claimed again.
const STALE: Duration = Duration::from_secs(5 * 60);

impl Db {
    pub(crate) async fn with_job_table(self) -> Result<Self> {
        self.role(crate::db::Role::Queue).execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_jobs\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"name\" TEXT NOT NULL, \
             \"queue\" TEXT NOT NULL, \"payload\" TEXT NOT NULL, \"run_at\" INTEGER NOT NULL, \"attempts\" INTEGER NOT NULL, \
             \"max_attempts\" INTEGER NOT NULL, \"status\" TEXT NOT NULL, \"locked_at\" INTEGER, \"last_error\" TEXT, \
             \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        // Columns added since the first version; tables made earlier gain them here.
        self.ensure_columns(
            "kiln_jobs",
            &[
                ("batch_id", "INTEGER"),
                ("batch_hook", "INTEGER NOT NULL DEFAULT 0"),
                ("priority", "INTEGER NOT NULL DEFAULT 0"),
                ("cancel_requested", "INTEGER NOT NULL DEFAULT 0"),
            ],
        )
        .await?;
        self.role(crate::db::Role::Queue)
            .execute("CREATE INDEX IF NOT EXISTS \"kiln_jobs_due\" ON \"kiln_jobs\" (\"status\", \"run_at\")")
            .await?;
        self.role(crate::db::Role::Queue).execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_queue_pauses\" (\"queue\" TEXT PRIMARY KEY, \"paused_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.role(crate::db::Role::Queue).execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_queue_limits\" (\"queue\" TEXT PRIMARY KEY, \"max_running\" INTEGER NOT NULL)",
        )
        .await?;
        self.with_schedule_tables()
            .await?
            .with_batch_table()
            .await?
            .with_notification_table()
            .await?
            .with_mail_table()
            .await
    }

    /// Adds `columns` to a Kiln table made before they existed.
    pub(crate) async fn ensure_columns(&self, table: &str, columns: &[(&str, &str)]) -> Result<()> {
        let existing: Vec<String> = self
            .role(crate::db::Role::Queue)
            .fetch_all(&self.dialect().columns_query(table), vec![])
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        for (name, definition) in columns {
            if !existing.iter().any(|e| e == name) {
                self.role(crate::db::Role::Queue)
                    .execute(&format!("ALTER TABLE \"{table}\" ADD COLUMN \"{name}\" {definition}"))
                    .await?;
            }
        }
        Ok(())
    }

    /// Claims the next due job (any due job when `all` is set, ignoring `run_at`): the
    /// highest priority first, then the longest due. Skips paused queues and queues at
    /// their concurrency limit, counted across every process.
    async fn claim_job(&self, all: bool) -> Result<Option<Claimed>> {
        let now = micros(Timestamp::now());
        let due = if all { i64::MAX } else { now };
        let stale = now - STALE.as_micros() as i64;
        self.reap_cancelled(stale).await?;
        let sql = format!(
            "UPDATE \"kiln_jobs\" SET \"status\" = 'running', \"locked_at\" = ?, \"attempts\" = \"attempts\" + 1 \
                 WHERE \"id\" = (SELECT j.\"id\" FROM \"kiln_jobs\" j \
                   WHERE ((j.\"status\" = 'queued' AND j.\"run_at\" <= ?) OR (j.\"status\" = 'running' AND j.\"locked_at\" < ?)) \
                   AND j.\"cancel_requested\" = 0 \
                   AND j.\"queue\" NOT IN (SELECT \"queue\" FROM \"kiln_queue_pauses\") \
                   AND NOT EXISTS (SELECT 1 FROM \"kiln_queue_limits\" l WHERE l.\"queue\" = j.\"queue\" \
                     AND (SELECT COUNT(*) FROM \"kiln_jobs\" r WHERE r.\"queue\" = l.\"queue\" AND r.\"status\" = 'running' \
                          AND r.\"locked_at\" >= ?) >= l.\"max_running\") \
                   ORDER BY j.\"priority\" DESC, j.\"run_at\", j.\"id\" LIMIT 1{lock}) \
                 RETURNING \"id\", \"name\", \"payload\", \"attempts\", \"max_attempts\", \"batch_id\", \"batch_hook\"",
            lock = self.dialect().claim_lock(),
        );
        let binds = vec![Value::Integer(now), Value::Integer(due), Value::Integer(stale), Value::Integer(stale)];
        // Where writes can run concurrently (Postgres), claims queue behind one lock, so the
        // per-queue running counts they check can't race.
        let rows = match self.dialect().serialize_claims() {
            Some(lock) => {
                self.role(crate::db::Role::Queue)
                    .transaction(|tx| async move {
                        tx.role(crate::db::Role::Queue).fetch_all(lock, vec![]).await?;
                        tx.role(crate::db::Role::Queue).fetch_all(&sql, binds).await
                    })
                    .await?
            }
            None => self.role(crate::db::Role::Queue).fetch_all(&sql, binds).await?,
        };
        Ok(rows.first().map(|r| Claimed {
            id: r.get(0),
            name: r.get(1),
            payload: r.get(2),
            attempts: r.get(3),
            max_attempts: r.get(4),
            batch: r.get::<Option<i64>, _>(5).map(BatchId),
            hook: r.get::<i64, _>(6) != 0,
        }))
    }

    async fn finish_job(&self, job: &Claimed, outcome: Outcome) -> Result<()> {
        let (id, batch) = (job.id, job.counted_batch());
        let outcome = match outcome {
            Outcome::Released => {
                self.role(crate::db::Role::Queue).fetch_all(
                    "UPDATE \"kiln_jobs\" SET \"status\" = 'queued', \"locked_at\" = NULL, \"attempts\" = CASE WHEN \"attempts\" > 0 THEN \"attempts\" - 1 ELSE 0 END \
                     WHERE \"id\" = ? RETURNING \"id\"",
                    vec![Value::Integer(id)],
                )
                .await?;
                return Ok(());
            }
            Outcome::Cancelled => {
                self.role(crate::db::Role::Queue)
                    .transaction(|tx| async move {
                        tx.role(crate::db::Role::Queue)
                            .fetch_all(
                                "DELETE FROM \"kiln_jobs\" WHERE \"id\" = ? RETURNING \"id\"",
                                vec![Value::Integer(id)],
                            )
                            .await?;
                        if let Some(batch) = batch {
                            batch::job_ended(&tx, batch, "cancelled").await?;
                        }
                        Ok(())
                    })
                    .await?;
                return Ok(());
            }
            Outcome::Done => Ok(()),
            Outcome::Failed(e) => Err(e),
        };
        match outcome {
            Ok(()) => {
                self.role(crate::db::Role::Queue)
                    .transaction(|tx| async move {
                        tx.role(crate::db::Role::Queue)
                            .fetch_all(
                                "DELETE FROM \"kiln_jobs\" WHERE \"id\" = ? RETURNING \"id\"",
                                vec![Value::Integer(id)],
                            )
                            .await?;
                        if let Some(batch) = batch {
                            batch::job_ended(&tx, batch, "succeeded").await?;
                        }
                        Ok(())
                    })
                    .await?;
            }
            Err(error) if job.attempts >= job.max_attempts => {
                tracing::error!(target: "kiln::jobs", id = job.id, name = %job.name, attempts = job.attempts, error = %error, "job failed for good");
                self.role(crate::db::Role::Queue).transaction(|tx| async move {
                    tx.role(crate::db::Role::Queue).fetch_all(
                        "UPDATE \"kiln_jobs\" SET \"status\" = 'failed', \"locked_at\" = NULL, \"last_error\" = ? WHERE \"id\" = ? RETURNING \"id\"",
                        vec![Value::Text(error), Value::Integer(id)],
                    )
                    .await?;
                    if let Some(batch) = batch {
                        batch::job_ended(&tx, batch, "failed").await?;
                    }
                    Ok(())
                })
                .await?;
            }
            Err(error) => {
                // 2s, 4s, 8s, ... capped at an hour.
                let delay = Duration::from_secs(2u64.saturating_pow(job.attempts.clamp(1, 12) as u32).min(3600));
                tracing::warn!(target: "kiln::jobs", id = job.id, name = %job.name, attempts = job.attempts, retry_in = ?delay, error = %error, "job failed; will retry");
                let run_at = micros(Timestamp::now()) + delay.as_micros() as i64;
                self.role(crate::db::Role::Queue).fetch_all(
                    "UPDATE \"kiln_jobs\" SET \"status\" = 'queued', \"locked_at\" = NULL, \"last_error\" = ?, \"run_at\" = ? WHERE \"id\" = ? RETURNING \"id\"",
                    vec![Value::Text(error), Value::Integer(run_at), Value::Integer(id)],
                )
                .await?;
            }
        }
        Ok(())
    }

    /// `(status, count)` for every status present.
    pub async fn job_counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all("SELECT \"status\", COUNT(*) FROM \"kiln_jobs\" GROUP BY \"status\" ORDER BY \"status\"", vec![])
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Failed jobs: `(id, name, attempts, last_error)`, newest first.
    pub async fn failed_jobs(&self) -> Result<Vec<(i64, String, i64, String)>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"id\", \"name\", \"attempts\", COALESCE(\"last_error\", '') FROM \"kiln_jobs\" WHERE \"status\" = 'failed' ORDER BY \"id\" DESC LIMIT 50",
                vec![],
            )
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))).collect())
    }

    /// Queues failed jobs (one id, or all) to run again with fresh attempts.
    pub async fn retry_jobs(&self, id: Option<i64>) -> Result<usize> {
        let now = micros(Timestamp::now());
        let (filter, mut binds) = match id {
            Some(id) => ("AND \"id\" = ?", vec![Value::Integer(id)]),
            None => ("", vec![]),
        };
        binds.insert(0, Value::Integer(now));
        let sql = format!(
            "UPDATE \"kiln_jobs\" SET \"status\" = 'queued', \"attempts\" = 0, \"run_at\" = ? WHERE \"status\" = 'failed' {filter} \
             RETURNING \"batch_id\", \"batch_hook\""
        );
        let count = self
            .role(crate::db::Role::Queue)
            .transaction(|tx| async move {
                let rows = tx.role(crate::db::Role::Queue).fetch_all(&sql, binds).await?;
                for row in &rows {
                    if let (Some(batch), 0) = (row.get::<Option<i64>, _>(0), row.get::<i64, _>(1)) {
                        batch::job_retried(&tx, BatchId(batch)).await?;
                    }
                }
                Ok(rows.len())
            })
            .await?;
        wake_workers();
        Ok(count)
    }

    /// Running jobs whose cancel was requested but whose worker is gone: removed, and
    /// counted as cancelled in their batch.
    async fn reap_cancelled(&self, stale: i64) -> Result<()> {
        self.role(crate::db::Role::Queue).transaction(|tx| async move {
            let rows = tx
                .role(crate::db::Role::Queue)
                .fetch_all(
                    "DELETE FROM \"kiln_jobs\" WHERE \"status\" = 'running' AND \"cancel_requested\" = 1 AND \"locked_at\" < ? \
                     RETURNING \"batch_id\", \"batch_hook\"",
                    vec![Value::Integer(stale)],
                )
                .await?;
            for row in rows {
                if let (Some(batch), 0) = (row.get::<Option<i64>, _>(0), row.get::<i64, _>(1)) {
                    batch::job_ended(&tx, BatchId(batch), "cancelled").await?;
                }
            }
            Ok(())
        })
        .await
    }

    /// Cancels a job. A waiting (or delayed) or failed job is removed at once; a running one
    /// is stopped by its worker within about a second, at its next `.await` (open
    /// transactions roll back), and then removed. Returns false if there's no such job.
    pub async fn cancel_job(&self, id: i64) -> Result<bool> {
        let requested = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "UPDATE \"kiln_jobs\" SET \"cancel_requested\" = 1 WHERE \"id\" = ? AND \"status\" = 'running' RETURNING \"id\"",
                vec![Value::Integer(id)],
            )
            .await?;
        if !requested.is_empty() {
            tracing::info!(target: "kiln::jobs", id, "cancel requested for a running job");
            return Ok(true);
        }
        self.role(crate::db::Role::Queue)
            .transaction(|tx| async move {
                let rows = tx
                    .role(crate::db::Role::Queue)
                    .fetch_all(
                        "DELETE FROM \"kiln_jobs\" WHERE \"id\" = ? AND \"status\" IN ('queued', 'failed') \
                     RETURNING \"status\", \"batch_id\", \"batch_hook\"",
                        vec![Value::Integer(id)],
                    )
                    .await?;
                let Some(row) = rows.first() else { return Ok(false) };
                // A cancelled waiting job no longer holds its batch open. (A failed one was
                // already counted as failed.)
                if let ("queued", Some(batch), 0) =
                    (row.get::<&str, _>(0), row.get::<Option<i64>, _>(1), row.get::<i64, _>(2))
                {
                    batch::job_ended(&tx, BatchId(batch), "cancelled").await?;
                }
                Ok(true)
            })
            .await
    }

    /// Deletes every failed job. Returns how many.
    pub async fn discard_failed_jobs(&self) -> Result<usize> {
        Ok(self
            .role(crate::db::Role::Queue)
            .fetch_all("DELETE FROM \"kiln_jobs\" WHERE \"status\" = 'failed' RETURNING \"id\"", vec![])
            .await?
            .len())
    }

    /// Makes a delayed job due now.
    pub async fn run_job_now(&self, id: i64) -> Result<bool> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "UPDATE \"kiln_jobs\" SET \"run_at\" = ? WHERE \"id\" = ? AND \"status\" = 'queued' RETURNING \"id\"",
                vec![Value::Integer(micros(Timestamp::now())), Value::Integer(id)],
            )
            .await?;
        wake_workers();
        Ok(!rows.is_empty())
    }

    /// Stops workers taking jobs from `queue` (running jobs finish). Jobs can still be
    /// enqueued; they wait.
    pub async fn pause_queue(&self, queue: &str) -> Result<()> {
        self.role(crate::db::Role::Queue).fetch_all(
            "INSERT INTO \"kiln_queue_pauses\" (\"queue\", \"paused_at\") VALUES (?, ?) ON CONFLICT DO NOTHING RETURNING \"queue\"",
            vec![Value::Text(queue.to_owned()), Value::Integer(micros(Timestamp::now()))],
        )
        .await?;
        Ok(())
    }

    pub async fn resume_queue(&self, queue: &str) -> Result<()> {
        self.role(crate::db::Role::Queue)
            .fetch_all(
                "DELETE FROM \"kiln_queue_pauses\" WHERE \"queue\" = ? RETURNING \"queue\"",
                vec![Value::Text(queue.to_owned())],
            )
            .await?;
        wake_workers();
        Ok(())
    }

    /// Every queue with jobs or a pause: `(queue, paused, queued, running, failed)`.
    pub async fn queues(&self) -> Result<Vec<(String, bool, i64, i64, i64)>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT q.\"queue\", CASE WHEN EXISTS (SELECT 1 FROM \"kiln_queue_pauses\" p WHERE p.\"queue\" = q.\"queue\") THEN 1 ELSE 0 END,\
                 (SELECT COUNT(*) FROM \"kiln_jobs\" j WHERE j.\"queue\" = q.\"queue\" AND j.\"status\" = 'queued'), \
                 (SELECT COUNT(*) FROM \"kiln_jobs\" j WHERE j.\"queue\" = q.\"queue\" AND j.\"status\" = 'running'), \
                 (SELECT COUNT(*) FROM \"kiln_jobs\" j WHERE j.\"queue\" = q.\"queue\" AND j.\"status\" = 'failed') \
                 FROM (SELECT \"queue\" FROM \"kiln_jobs\" UNION SELECT \"queue\" FROM \"kiln_queue_pauses\" \
                       UNION SELECT \"queue\" FROM \"kiln_schedules\") q ORDER BY q.\"queue\"",
                vec![],
            )
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get::<i64, _>(1) != 0, r.get(2), r.get(3), r.get(4))).collect())
    }

    /// Payloads of queued jobs named `name`, oldest first.
    pub(crate) async fn queued_payloads(&self, name: &str) -> Result<Vec<String>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"payload\" FROM \"kiln_jobs\" WHERE \"name\" = ? AND \"status\" = 'queued' ORDER BY \"id\"",
                vec![Value::Text(name.to_owned())],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }
}

/// What `work_one` did.
enum Worked {
    Idle,
    Ran,
    /// The job used its last attempt: `"Name: error"`.
    FailedForGood(String),
}

/// Claims and runs one job.
async fn work_one(ctx: &Ctx, all: bool) -> Result<Worked> {
    if shutting_down() {
        return Ok(Worked::Idle);
    }
    let Some(job) = ctx.db.claim_job(all).await? else { return Ok(Worked::Idle) };
    let outcome = match job_def(&job.name) {
        None => Outcome::Failed(format!("no job named `{}` is linked into this binary", job.name)),
        Some(def) => match serde_json::from_str(&job.payload) {
            Err(e) => Outcome::Failed(format!("unreadable payload: {e}")),
            Ok(payload) => {
                // Spawned so a panic fails the job instead of the worker, and so the watcher
                // can stop it: dropping a future ends it at its next `.await`.
                let run = CURRENT_BATCH.scope(job.batch, (def.run)(ctx.clone(), payload));
                // A test's moved clock (`app.travel`) follows the job into its task.
                let handle = match crate::types::CLOCK_OFFSET.try_with(Clone::clone) {
                    Ok(clock) => tokio::spawn(crate::types::CLOCK_OFFSET.scope(clock, run)),
                    Err(_) => tokio::spawn(run),
                };
                watch_attempt(ctx, &job, def.timeout, handle).await
            }
        },
    };
    let worked = match &outcome {
        Outcome::Done => {
            tracing::info!(target: "kiln::jobs", id = job.id, name = %job.name, attempts = job.attempts, "job done");
            Worked::Ran
        }
        Outcome::Failed(e) if job.attempts >= job.max_attempts => Worked::FailedForGood(format!("{}: {e}", job.name)),
        Outcome::Cancelled => {
            tracing::info!(target: "kiln::jobs", id = job.id, name = %job.name, "job cancelled while running");
            Worked::Ran
        }
        Outcome::Released => {
            tracing::info!(target: "kiln::jobs", id = job.id, name = %job.name, "job released by shutdown; it will run again");
            Worked::Ran
        }
        Outcome::Failed(_) => Worked::Ran,
    };
    ctx.db.finish_job(&job, outcome).await?;
    Ok(worked)
}

/// Waits for a running attempt, stopping it when it's cancelled, runs past its time limit,
/// or outlives a shutdown's grace period. Refreshes the job's lock meanwhile, so long jobs
/// aren't mistaken for abandoned ones.
async fn watch_attempt(
    ctx: &Ctx,
    job: &Claimed,
    timeout: Option<i64>,
    mut handle: tokio::task::JoinHandle<Result>,
) -> Outcome {
    let started = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.tick().await;
    let mut last_heartbeat = std::time::Instant::now();
    loop {
        tokio::select! {
            joined = &mut handle => {
                return match joined {
                    Ok(Ok(())) => Outcome::Done,
                    Ok(Err(e)) => Outcome::Failed(e.to_string()),
                    Err(join) if join.is_cancelled() => Outcome::Cancelled,
                    Err(join) => Outcome::Failed(format!("the job panicked: {join}")),
                };
            }
            _ = tick.tick() => {
                if let Some(limit) = timeout
                    && started.elapsed() >= Duration::from_secs(limit as u64)
                {
                    handle.abort();
                    return Outcome::Failed(format!("timed out after {limit}s"));
                }
                if shutdown_deadline_passed() {
                    handle.abort();
                    return Outcome::Released;
                }
                let heartbeat = last_heartbeat.elapsed() >= Duration::from_secs(60);
                let sql = if heartbeat {
                    "UPDATE \"kiln_jobs\" SET \"locked_at\" = ? WHERE \"id\" = ? RETURNING \"cancel_requested\""
                } else {
                    "SELECT \"cancel_requested\" FROM \"kiln_jobs\" WHERE \"id\" = ?"
                };
                let binds = if heartbeat {
                    last_heartbeat = std::time::Instant::now();
                    vec![Value::Integer(micros(Timestamp::now())), Value::Integer(job.id)]
                } else {
                    vec![Value::Integer(job.id)]
                };
                let cancel = ctx.db.role(crate::db::Role::Queue).fetch_all(sql, binds).await.ok().and_then(|rows| rows.first().map(|r| r.get::<i64, _>(0) != 0));
                if cancel == Some(true) {
                    handle.abort();
                    return Outcome::Cancelled;
                }
            }
        }
    }
}

/// Shutdown: `None` while running; once a signal arrives, when running jobs must stop.
static SHUTDOWN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn shutdown_hub() -> &'static watch::Sender<bool> {
    static HUB: OnceLock<watch::Sender<bool>> = OnceLock::new();
    HUB.get_or_init(|| watch::channel(false).0)
}

fn shutting_down() -> bool {
    SHUTDOWN.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

fn shutdown_deadline_passed() -> bool {
    SHUTDOWN.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|deadline| std::time::Instant::now() >= deadline)
}

/// How long running jobs get to finish after SIGTERM: `JOB_SHUTDOWN_GRACE` seconds, by
/// default 25 in production (under the usual 30 s kill) and 2 in development.
fn shutdown_grace() -> Duration {
    let default = if crate::config::is_production() { 25 } else { 2 };
    Duration::from_secs(std::env::var("JOB_SHUTDOWN_GRACE").ok().and_then(|v| v.parse().ok()).unwrap_or(default))
}

/// Starts a shutdown: workers stop claiming, running jobs get the grace period, then are
/// stopped and put back in the queue.
pub fn begin_shutdown() {
    let mut deadline = SHUTDOWN.lock().unwrap_or_else(|e| e.into_inner());
    if deadline.is_none() {
        *deadline = Some(std::time::Instant::now() + shutdown_grace());
        shutdown_hub().send_replace(true);
    }
}

/// Resolves once a shutdown starts (for the web server's graceful shutdown).
pub async fn shutdown_requested() {
    let mut rx = shutdown_hub().subscribe();
    let _ = rx.wait_for(|down| *down).await;
}

/// Under `kiln dev`, stdin is a pipe from it: when that closes (kiln dev is gone, even if
/// killed outright), shut down rather than keep holding the port.
pub(crate) fn exit_with_kiln_dev() {
    if std::env::var("KILN_DEV_STDIN_WATCH").as_deref() != Ok("1") {
        return;
    }
    static WATCHING: OnceLock<()> = OnceLock::new();
    if WATCHING.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        use std::io::Read;
        let mut buf = [0u8; 64];
        while matches!(std::io::stdin().read(&mut buf), Ok(n) if n > 0) {}
        // No logging from here on: kiln dev was reading our stdout and stderr, so writing
        // to them now fails, and a failed `eprintln!` (which log subscribers use to report
        // write errors) panics. Shut down quietly instead.
        begin_shutdown();
        std::thread::sleep(shutdown_grace());
        std::process::exit(0);
    });
}

/// Turns SIGTERM and Ctrl-C into [`begin_shutdown`], once per process. If the process
/// is still alive well after the grace period, it exits.
pub(crate) fn listen_for_shutdown() {
    static LISTENING: OnceLock<()> = OnceLock::new();
    if LISTENING.set(()).is_err() {
        return;
    }
    tokio::spawn(async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
        tracing::info!(target: "kiln::jobs", grace = ?shutdown_grace(), "shutting down: finishing running jobs");
        begin_shutdown();
        tokio::time::sleep(shutdown_grace() + Duration::from_secs(5)).await;
        tracing::warn!(target: "kiln::jobs", "still running after the grace period; exiting");
        std::process::exit(0);
    });
}

/// Runs jobs forever with `concurrency` workers, plus the app's schedule.
pub async fn work(ctx: Ctx, concurrency: usize) {
    listen_for_shutdown();
    exit_with_kiln_dev();
    if let Err(e) = sync_app(&ctx).await {
        tracing::error!(target: "kiln::jobs", error = %e, "syncing schedules and queue limits failed");
    }
    // Upkeep: files no record kept (forms never saved, files replaced) go after two days,
    // expired cache entries every few minutes.
    let upkeep = ctx.clone();
    tokio::spawn(async move {
        let mut ticks: u64 = 0;
        while !shutting_down() {
            if ticks.is_multiple_of(12)
                && let Err(e) = crate::storage::purge_unattached(&upkeep, jiff::SignedDuration::from_hours(48)).await
            {
                tracing::error!(target: "kiln::storage", error = %e, "removing unattached files failed");
            }
            if let Err(e) = clear_stale_pending(&upkeep.db).await {
                tracing::error!(target: "kiln::jobs", error = %e, "clearing stale pending jobs failed");
            }
            if let Err(e) = crate::cache::purge_expired(&upkeep.db).await {
                tracing::error!(target: "kiln::cache", error = %e, "removing expired cache entries failed");
            }
            ticks += 1;
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
    if ctx.try_get::<crate::Schedule>().is_some() {
        let db = ctx.db.clone();
        tokio::spawn(async move {
            while !shutting_down() {
                if let Err(e) = db.run_due_schedules(Timestamp::now()).await {
                    tracing::error!(target: "kiln::schedule", error = %e, "running schedules failed");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
    let workers = (0..concurrency.max(1)).map(|_| {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut wake = hub().subscribe();
            let mut down = shutdown_hub().subscribe();
            while !shutting_down() {
                match work_one(&ctx, false).await {
                    Ok(Worked::Ran | Worked::FailedForGood(_)) => continue,
                    Ok(Worked::Idle) => {}
                    Err(e) => tracing::error!(target: "kiln::jobs", error = %e, "worker error"),
                }
                // Jobs from this process wake us; other processes' jobs are found by polling.
                tokio::select! {
                    _ = wake.changed() => {}
                    _ = down.changed() => {}
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        })
    });
    for worker in workers.collect::<Vec<_>>() {
        let _ = worker.await;
    }
}

/// Runs every queued job, including ones they enqueue, regardless of `run_at`. For tests:
/// a job that uses up its attempts is an error naming it (after the rest have run).
pub async fn perform_all(ctx: &Ctx) -> Result<usize> {
    perform(ctx, true).await
}

/// Like [`perform_all`], but only jobs due by `Timestamp::now()` (which tests can move).
pub async fn perform_due(ctx: &Ctx) -> Result<usize> {
    perform(ctx, false).await
}

async fn perform(ctx: &Ctx, all: bool) -> Result<usize> {
    let mut done = 0;
    let mut failed = vec![];
    loop {
        match work_one(ctx, all).await? {
            Worked::Idle => break,
            Worked::Ran => done += 1,
            Worked::FailedForGood(e) => failed.push(e),
        }
        if done + failed.len() > 10_000 {
            return Err(Error::Integrity("jobs keep enqueueing jobs; stopped after 10,000".into()));
        }
    }
    if failed.is_empty() {
        Ok(done)
    } else {
        Err(Error::Job(format!("job(s) failed after their last attempt:\n  {}", failed.join("\n  "))))
    }
}
