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
            insert_job(db, Self::NAME, Self::QUEUE, payload, micros(at), Self::ATTEMPTS).await
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
) -> Result<i64> {
    insert_row(db, name, queue, payload, run_at, max_attempts, None, false).await
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
    batch: Option<BatchId>,
    hook: bool,
) -> Result<i64> {
    let rows = db
        .fetch_all(
            "INSERT INTO \"kiln_jobs\" (\"name\", \"queue\", \"payload\", \"run_at\", \"attempts\", \"max_attempts\", \"status\", \"created_at\", \"batch_id\", \"batch_hook\") \
             VALUES (?, ?, ?, ?, 0, ?, 'queued', ?, ?, ?) RETURNING \"id\"",
            vec![
                Value::Text(name.to_owned()),
                Value::Text(queue.to_owned()),
                Value::Text(payload),
                Value::Integer(run_at),
                Value::Integer(max_attempts),
                Value::Integer(micros(Timestamp::now())),
                batch.map_or(Value::Null, |b| Value::Integer(b.0)),
                Value::Integer(hook as i64),
            ],
        )
        .await?;
    db.after_commit(wake_workers);
    Ok(rows.first().map(|r| r.get(0)).unwrap_or_default())
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

fn wake_workers() {
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
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_jobs\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"name\" TEXT NOT NULL, \
             \"queue\" TEXT NOT NULL, \"payload\" TEXT NOT NULL, \"run_at\" INTEGER NOT NULL, \"attempts\" INTEGER NOT NULL, \
             \"max_attempts\" INTEGER NOT NULL, \"status\" TEXT NOT NULL, \"locked_at\" INTEGER, \"last_error\" TEXT, \
             \"created_at\" INTEGER NOT NULL, \"batch_id\" INTEGER, \"batch_hook\" INTEGER NOT NULL DEFAULT 0)",
        )
        .await?;
        // Tables made before batches existed.
        let columns = self.fetch_all("SELECT \"name\" FROM pragma_table_info('kiln_jobs')", vec![]).await?;
        if !columns.iter().any(|r| r.get::<String, _>(0) == "batch_id") {
            self.execute("ALTER TABLE \"kiln_jobs\" ADD COLUMN \"batch_id\" INTEGER").await?;
            self.execute("ALTER TABLE \"kiln_jobs\" ADD COLUMN \"batch_hook\" INTEGER NOT NULL DEFAULT 0").await?;
        }
        self.execute("CREATE INDEX IF NOT EXISTS \"kiln_jobs_due\" ON \"kiln_jobs\" (\"status\", \"run_at\")").await?;
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_queue_pauses\" (\"queue\" TEXT PRIMARY KEY, \"paused_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.with_schedule_tables().await?.with_batch_table().await?.with_mail_table().await
    }

    /// Claims the next due job (any due job when `all` is set, ignoring `run_at`).
    async fn claim_job(&self, all: bool) -> Result<Option<Claimed>> {
        let now = micros(Timestamp::now());
        let due = if all { i64::MAX } else { now };
        let stale = now - STALE.as_micros() as i64;
        let rows = self
            .fetch_all(
                "UPDATE \"kiln_jobs\" SET \"status\" = 'running', \"locked_at\" = ?, \"attempts\" = \"attempts\" + 1 \
                 WHERE \"id\" = (SELECT \"id\" FROM \"kiln_jobs\" \
                   WHERE ((\"status\" = 'queued' AND \"run_at\" <= ?) OR (\"status\" = 'running' AND \"locked_at\" < ?)) \
                   AND \"queue\" NOT IN (SELECT \"queue\" FROM \"kiln_queue_pauses\") \
                   ORDER BY \"run_at\", \"id\" LIMIT 1) \
                 RETURNING \"id\", \"name\", \"payload\", \"attempts\", \"max_attempts\", \"batch_id\", \"batch_hook\"",
                vec![Value::Integer(now), Value::Integer(due), Value::Integer(stale)],
            )
            .await?;
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

    async fn finish_job(&self, job: &Claimed, outcome: std::result::Result<(), String>) -> Result<()> {
        let (id, batch) = (job.id, job.counted_batch());
        match outcome {
            Ok(()) => {
                self.transaction(|tx| async move {
                    tx.fetch_all(
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
                self.transaction(|tx| async move {
                    tx.fetch_all(
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
                self.fetch_all(
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
            .fetch_all("SELECT \"status\", COUNT(*) FROM \"kiln_jobs\" GROUP BY \"status\" ORDER BY \"status\"", vec![])
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Failed jobs: `(id, name, attempts, last_error)`, newest first.
    pub async fn failed_jobs(&self) -> Result<Vec<(i64, String, i64, String)>> {
        let rows = self
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
            .transaction(|tx| async move {
                let rows = tx.fetch_all(&sql, binds).await?;
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

    /// Removes a job that hasn't started: queued (including delayed) or failed. Running
    /// jobs can't be cancelled; they finish or fail.
    pub async fn cancel_job(&self, id: i64) -> Result<bool> {
        self.transaction(|tx| async move {
            let rows = tx
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
            .fetch_all("DELETE FROM \"kiln_jobs\" WHERE \"status\" = 'failed' RETURNING \"id\"", vec![])
            .await?
            .len())
    }

    /// Makes a delayed job due now.
    pub async fn run_job_now(&self, id: i64) -> Result<bool> {
        let rows = self
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
        self.fetch_all(
            "INSERT INTO \"kiln_queue_pauses\" (\"queue\", \"paused_at\") VALUES (?, ?) ON CONFLICT DO NOTHING RETURNING \"queue\"",
            vec![Value::Text(queue.to_owned()), Value::Integer(micros(Timestamp::now()))],
        )
        .await?;
        Ok(())
    }

    pub async fn resume_queue(&self, queue: &str) -> Result<()> {
        self.fetch_all(
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
            .fetch_all(
                "SELECT q.\"queue\", EXISTS (SELECT 1 FROM \"kiln_queue_pauses\" p WHERE p.\"queue\" = q.\"queue\"), \
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
    let Some(job) = ctx.db.claim_job(all).await? else { return Ok(Worked::Idle) };
    let outcome = match job_def(&job.name) {
        None => Err(format!("no job named `{}` is linked into this binary", job.name)),
        Some(def) => match serde_json::from_str(&job.payload) {
            Err(e) => Err(format!("unreadable payload: {e}")),
            // Spawned so a panic fails the job instead of the worker.
            Ok(payload) => match tokio::spawn(CURRENT_BATCH.scope(job.batch, (def.run)(ctx.clone(), payload))).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(join) => Err(format!("the job panicked: {join}")),
            },
        },
    };
    let worked = match &outcome {
        Ok(()) => {
            tracing::info!(target: "kiln::jobs", id = job.id, name = %job.name, attempts = job.attempts, "job done");
            Worked::Ran
        }
        Err(e) if job.attempts >= job.max_attempts => Worked::FailedForGood(format!("{}: {e}", job.name)),
        Err(_) => Worked::Ran,
    };
    ctx.db.finish_job(&job, outcome).await?;
    Ok(worked)
}

/// Runs jobs forever with `concurrency` workers, plus the app's schedule.
pub async fn work(ctx: Ctx, concurrency: usize) {
    if let Some(schedule) = ctx.try_get::<crate::Schedule>().cloned() {
        let db = ctx.db.clone();
        tokio::spawn(async move {
            if let Err(e) = db.sync_schedules(&schedule, Timestamp::now()).await {
                tracing::error!(target: "kiln::schedule", error = %e, "syncing schedules failed");
                return;
            }
            loop {
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
            loop {
                match work_one(&ctx, false).await {
                    Ok(Worked::Ran | Worked::FailedForGood(_)) => continue,
                    Ok(Worked::Idle) => {}
                    Err(e) => tracing::error!(target: "kiln::jobs", error = %e, "worker error"),
                }
                // Jobs from this process wake us; other processes' jobs are found by polling.
                tokio::select! {
                    _ = wake.changed() => {}
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
    let mut done = 0;
    let mut failed = vec![];
    loop {
        match work_one(ctx, true).await? {
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
