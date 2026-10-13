//! Job batches: a group of jobs tracked together, with hooks when they're done.
//!
//! ```ignore
//! let batch = Batch::new("Import October orders")
//!     .jobs(rows.iter().map(|r| ImportRow { row_id: r.id }))
//!     .on_success(ImportFinished { import_id })    // every job succeeded
//!     .on_failure(ImportFailed { import_id })      // the first job that fails for good
//!     .on_complete(CleanUpImport { import_id })    // every job finished, either way
//!     .on_cancel(ImportCancelled { import_id })    // the batch was cancelled
//!     .enqueue(&tx)
//!     .await?;
//! ```
//!
//! Hooks are ordinary jobs. Each fires at most once, enqueued in the same transaction as
//! the job update that triggered it. A failed job that's retried counts as pending again,
//! so a batch can still succeed after a fix. Cancelling removes the batch's waiting jobs
//! (running ones finish) and fires `on_cancel`; success and completion never fire after.
//!
//! Inside a batch's job or hook, [`BatchId::current`] is the batch: jobs can add more
//! work to it with `add`, and hooks can read `status`.

use serde::Serialize;
use sqlx::Row;

use crate::{
    Db, Error, Result, Timestamp, Value,
    jobs::{JobInfo, insert_job_in_batch},
};

/// A batch's id. Store it on a record (`pub import_batch: Option<BatchId>`) to show progress
/// later with `ctx.db.batch(id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct BatchId(pub i64);

impl std::fmt::Display for BatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl crate::types::ToValue for BatchId {
    fn to_value(&self) -> Value {
        Value::Integer(self.0)
    }
}

impl crate::types::FromColumn for BatchId {
    fn from_column(row: &crate::Row, name: &str) -> std::result::Result<Self, sqlx::Error> {
        row.try_get::<i64, _>(name).map(BatchId)
    }
}

impl crate::types::SqlType for BatchId {
    const SQL: &'static str = "INTEGER";
}

tokio::task_local! {
    /// The batch of the job (or hook) this task is running.
    pub(crate) static CURRENT_BATCH: Option<BatchId>;
}

/// A serialized job, ready to insert.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub(crate) struct JobSpec {
    pub name: String,
    pub queue: String,
    pub payload: String,
    pub max_attempts: i64,
    #[serde(default)]
    pub priority: i64,
}

impl JobSpec {
    fn of<J: JobInfo>(job: &J) -> Result<Self> {
        let payload = serde_json::to_string(job)
            .map_err(|e| Error::BadRequest(format!("job `{}` doesn't serialize: {e}", J::NAME)))?;
        Ok(Self {
            name: J::NAME.to_owned(),
            queue: J::QUEUE.to_owned(),
            payload,
            max_attempts: J::ATTEMPTS,
            priority: J::PRIORITY,
        })
    }
}

/// The hooks a batch can have, in the order they're stored.
const HOOKS: [&str; 4] = ["success", "failure", "complete", "cancel"];

/// A batch being built. Nothing is stored until [`Batch::enqueue`].
#[must_use = "a batch does nothing until `.enqueue(&tx).await?`"]
pub struct Batch {
    description: String,
    jobs: Vec<Result<JobSpec>>,
    hooks: [Option<Result<JobSpec>>; 4],
    progress: Option<Result<JobSpec>>,
}

impl Batch {
    pub fn new(description: impl Into<String>) -> Self {
        Self { description: description.into(), jobs: vec![], hooks: [None, None, None, None], progress: None }
    }

    pub fn job<J: JobInfo>(mut self, job: J) -> Self {
        self.jobs.push(JobSpec::of(&job));
        self
    }

    pub fn jobs<J: JobInfo>(mut self, jobs: impl IntoIterator<Item = J>) -> Self {
        self.jobs.extend(jobs.into_iter().map(|j| JobSpec::of(&j)));
        self
    }

    /// Enqueued once every job has succeeded.
    pub fn on_success<J: JobInfo>(mut self, job: J) -> Self {
        self.hooks[0] = Some(JobSpec::of(&job));
        self
    }

    /// Enqueued the first time a job in the batch fails for good.
    pub fn on_failure<J: JobInfo>(mut self, job: J) -> Self {
        self.hooks[1] = Some(JobSpec::of(&job));
        self
    }

    /// Enqueued once every job has finished, whether it succeeded or failed.
    pub fn on_complete<J: JobInfo>(mut self, job: J) -> Self {
        self.hooks[2] = Some(JobSpec::of(&job));
        self
    }

    /// Enqueued after the batch's jobs finish, to report progress (broadcast a progress bar,
    /// say). Inside it, `BatchId::current()` and `status` already count the job that just
    /// finished. At most one waits at a time, so a big batch doesn't queue one per job.
    pub fn on_progress<J: JobInfo>(mut self, job: J) -> Self {
        self.progress = Some(JobSpec::of(&job));
        self
    }

    /// Enqueued when the batch is cancelled.
    pub fn on_cancel<J: JobInfo>(mut self, job: J) -> Self {
        self.hooks[3] = Some(JobSpec::of(&job));
        self
    }

    /// Stores the batch and its jobs. Inside a transaction, all of it or none of it. An
    /// empty batch completes (and succeeds) at once.
    pub async fn enqueue(self, db: &Db) -> Result<BatchId> {
        let jobs: Vec<JobSpec> = self.jobs.into_iter().collect::<Result<_>>()?;
        let mut hooks = serde_json::Map::new();
        for (name, hook) in HOOKS.iter().zip(self.hooks) {
            if let Some(hook) = hook {
                hooks.insert((*name).to_owned(), serde_json::to_value(hook?).expect("job specs serialize"));
            }
        }
        if let Some(progress) = self.progress {
            hooks.insert("progress".to_owned(), serde_json::to_value(progress?).expect("job specs serialize"));
        }
        let description = self.description;
        db.transaction(|tx| async move {
            let rows = tx
                .fetch_all(
                    "INSERT INTO \"kiln_batches\" (\"description\", \"total\", \"pending\", \"hooks\", \"status\", \"created_at\") \
                     VALUES (?, ?, ?, ?, 'running', ?) RETURNING \"id\"",
                    vec![
                        Value::Text(description),
                        Value::Integer(jobs.len() as i64),
                        Value::Integer(jobs.len() as i64),
                        Value::Text(serde_json::Value::Object(hooks).to_string()),
                        Value::Integer(now()),
                    ],
                )
                .await?;
            let id = BatchId(rows.first().map(|r| r.get(0)).unwrap_or_default());
            for job in jobs {
                insert_job_in_batch(&tx, &job, Some(id), false).await?;
            }
            settle(&tx, id).await?;
            Ok(id)
        })
        .await
    }
}

fn now() -> i64 {
    Timestamp::now().as_jiff().as_microsecond()
}

/// Where a batch stands.
#[derive(Debug, Clone, Serialize)]
pub struct BatchStatus {
    pub id: BatchId,
    pub description: String,
    /// `running`, `finished` or `cancelled`.
    pub status: String,
    pub total: i64,
    pub pending: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub cancelled: i64,
    pub created_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    /// Hook names and job names: `("success", "ImportFinished", fired)`.
    pub hooks: Vec<(String, String, bool)>,
}

impl BatchStatus {
    pub fn succeeded_entirely(&self) -> bool {
        self.status == "finished" && self.failed == 0 && self.cancelled == 0
    }
}

impl BatchId {
    /// The batch of the job or hook running in this task.
    pub fn current() -> Option<BatchId> {
        CURRENT_BATCH.try_with(|b| *b).ok().flatten()
    }

    /// Adds a job to a running batch (from one of its own jobs, say, to fan out).
    pub async fn add<J: JobInfo>(&self, db: &Db, job: J) -> Result<i64> {
        let spec = JobSpec::of(&job)?;
        let id = *self;
        db.transaction(|tx| async move {
            let open = tx
                .fetch_all(
                    "UPDATE \"kiln_batches\" SET \"total\" = \"total\" + 1, \"pending\" = \"pending\" + 1 \
                     WHERE \"id\" = ? AND \"status\" = 'running' RETURNING \"id\"",
                    vec![Value::Integer(id.0)],
                )
                .await?;
            if open.is_empty() {
                return Err(Error::Job(format!("batch {id} is finished or cancelled; it takes no more jobs")));
            }
            insert_job_in_batch(&tx, &spec, Some(id), false).await
        })
        .await
    }

    pub async fn status(&self, db: &Db) -> Result<BatchStatus> {
        db.batch(*self).await?.ok_or(Error::NotFound)
    }

    /// Cancels the batch: deletes its waiting jobs and fires `on_cancel`. Running jobs
    /// finish. Returns false if the batch had already ended.
    pub async fn cancel(&self, db: &Db) -> Result<bool> {
        let id = *self;
        db.transaction(|tx| async move {
            let ended = tx
                .fetch_all(
                    "UPDATE \"kiln_batches\" SET \"status\" = 'cancelled', \"finished_at\" = ? \
                     WHERE \"id\" = ? AND \"status\" = 'running' RETURNING \"id\"",
                    vec![Value::Integer(now()), Value::Integer(id.0)],
                )
                .await?;
            if ended.is_empty() {
                return Ok(false);
            }
            let removed = tx
                .fetch_all(
                    "DELETE FROM \"kiln_jobs\" WHERE \"batch_id\" = ? AND \"batch_hook\" = 0 AND \"status\" = 'queued' RETURNING \"id\"",
                    vec![Value::Integer(id.0)],
                )
                .await?;
            let n = removed.len() as i64;
            tx.fetch_all(
                "UPDATE \"kiln_batches\" SET \"pending\" = \"pending\" - ?, \"cancelled\" = \"cancelled\" + ? WHERE \"id\" = ? RETURNING \"id\"",
                vec![Value::Integer(n), Value::Integer(n), Value::Integer(id.0)],
            )
            .await?;
            fire(&tx, id, "cancel").await?;
            Ok(true)
        })
        .await
    }
}

/// Records how one of the batch's jobs ended (`succeeded`, `failed` or `cancelled`), then
/// fires whatever hooks are now due. Runs inside the job's own update transaction.
pub(crate) async fn job_ended(tx: &Db, batch: BatchId, outcome: &str) -> Result<()> {
    let column = match outcome {
        "succeeded" => "succeeded",
        "failed" => "failed",
        _ => "cancelled",
    };
    tx.fetch_all(
        &format!("UPDATE \"kiln_batches\" SET \"pending\" = \"pending\" - 1, \"{column}\" = \"{column}\" + 1 WHERE \"id\" = ? RETURNING \"id\""),
        vec![Value::Integer(batch.0)],
    )
    .await?;
    if outcome == "failed" {
        fire(tx, batch, "failure").await?;
    }
    progress(tx, batch).await?;
    settle(tx, batch).await
}

/// Enqueues the batch's `on_progress` job, unless one is already waiting.
async fn progress(tx: &Db, batch: BatchId) -> Result<()> {
    let rows =
        tx.fetch_all("SELECT \"hooks\" FROM \"kiln_batches\" WHERE \"id\" = ?", vec![Value::Integer(batch.0)]).await?;
    let Some(row) = rows.first() else { return Ok(()) };
    let hooks: serde_json::Value = serde_json::from_str(&row.get::<String, _>(0)).unwrap_or_default();
    let Some(spec) = hooks.get("progress").and_then(|h| serde_json::from_value::<JobSpec>(h.clone()).ok()) else {
        return Ok(());
    };
    let waiting = tx
        .fetch_all(
            "SELECT 1 FROM \"kiln_jobs\" WHERE \"batch_id\" = ? AND \"batch_hook\" = 1 AND \"name\" = ? AND \"status\" = 'queued'",
            vec![Value::Integer(batch.0), Value::Text(spec.name.clone())],
        )
        .await?;
    if waiting.is_empty() {
        insert_job_in_batch(tx, &spec, Some(batch), true).await?;
    }
    Ok(())
}

/// A failed job of the batch was queued again.
pub(crate) async fn job_retried(tx: &Db, batch: BatchId) -> Result<()> {
    tx.fetch_all(
        "UPDATE \"kiln_batches\" SET \"pending\" = \"pending\" + 1, \"failed\" = \"failed\" - 1, \"status\" = 'running', \"finished_at\" = NULL \
         WHERE \"id\" = ? AND \"status\" != 'cancelled' RETURNING \"id\"",
        vec![Value::Integer(batch.0)],
    )
    .await?;
    Ok(())
}

/// Finishes the batch when nothing is pending, firing `complete` and (if nothing failed)
/// `success`.
async fn settle(tx: &Db, batch: BatchId) -> Result<()> {
    let done = tx
        .fetch_all(
            "UPDATE \"kiln_batches\" SET \"status\" = 'finished', \"finished_at\" = ? \
             WHERE \"id\" = ? AND \"status\" = 'running' AND \"pending\" <= 0 RETURNING \"failed\", \"cancelled\"",
            vec![Value::Integer(now()), Value::Integer(batch.0)],
        )
        .await?;
    let Some(row) = done.first() else { return Ok(()) };
    let (failed, cancelled): (i64, i64) = (row.get(0), row.get(1));
    if failed == 0 && cancelled == 0 {
        fire(tx, batch, "success").await?;
    }
    fire(tx, batch, "complete").await
}

/// Enqueues the batch's `hook` job unless it has fired before.
async fn fire(tx: &Db, batch: BatchId, hook: &str) -> Result<()> {
    let column = format!("fired_{hook}");
    let won = tx
        .fetch_all(
            &format!(
                "UPDATE \"kiln_batches\" SET \"{column}\" = 1 WHERE \"id\" = ? AND \"{column}\" = 0 RETURNING \"hooks\""
            ),
            vec![Value::Integer(batch.0)],
        )
        .await?;
    let Some(row) = won.first() else { return Ok(()) };
    let hooks: serde_json::Value = serde_json::from_str(&row.get::<String, _>(0)).unwrap_or_default();
    if let Some(spec) = hooks.get(hook).and_then(|h| serde_json::from_value::<JobSpec>(h.clone()).ok()) {
        tracing::info!(target: "kiln::jobs", batch = batch.0, hook, job = %spec.name, "batch hook enqueued");
        insert_job_in_batch(tx, &spec, Some(batch), true).await?;
    }
    Ok(())
}

impl Db {
    pub(crate) async fn with_batch_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_batches\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"description\" TEXT NOT NULL, \
             \"status\" TEXT NOT NULL, \"total\" INTEGER NOT NULL, \"pending\" INTEGER NOT NULL, \
             \"succeeded\" INTEGER NOT NULL DEFAULT 0, \"failed\" INTEGER NOT NULL DEFAULT 0, \"cancelled\" INTEGER NOT NULL DEFAULT 0, \
             \"hooks\" TEXT NOT NULL, \"fired_success\" INTEGER NOT NULL DEFAULT 0, \"fired_failure\" INTEGER NOT NULL DEFAULT 0, \
             \"fired_complete\" INTEGER NOT NULL DEFAULT 0, \"fired_cancel\" INTEGER NOT NULL DEFAULT 0, \
             \"created_at\" INTEGER NOT NULL, \"finished_at\" INTEGER)",
        )
        .await?;
        Ok(self)
    }

    pub async fn batch(&self, id: BatchId) -> Result<Option<BatchStatus>> {
        Ok(self.batches_where("WHERE \"id\" = ?", vec![Value::Integer(id.0)]).await?.into_iter().next())
    }

    /// The newest batches first.
    pub async fn batches(&self, limit: i64) -> Result<Vec<BatchStatus>> {
        self.batches_where("ORDER BY \"id\" DESC LIMIT ?", vec![Value::Integer(limit)]).await
    }

    async fn batches_where(&self, clause: &str, binds: Vec<Value>) -> Result<Vec<BatchStatus>> {
        let rows = self
            .fetch_all(
                &format!(
                    "SELECT \"id\", \"description\", \"status\", \"total\", \"pending\", \"succeeded\", \"failed\", \"cancelled\", \
                     \"created_at\", \"finished_at\", \"hooks\", \"fired_success\", \"fired_failure\", \"fired_complete\", \"fired_cancel\" \
                     FROM \"kiln_batches\" {clause}"
                ),
                binds,
            )
            .await?;
        let at = |us: i64| Timestamp::from_jiff(jiff::Timestamp::from_microsecond(us).unwrap_or_default());
        Ok(rows
            .iter()
            .map(|r| {
                let hooks: serde_json::Value = serde_json::from_str(&r.get::<String, _>(10)).unwrap_or_default();
                let fired = [r.get::<i64, _>(11), r.get(12), r.get(13), r.get(14)];
                BatchStatus {
                    id: BatchId(r.get(0)),
                    description: r.get(1),
                    status: r.get(2),
                    total: r.get(3),
                    pending: r.get(4),
                    succeeded: r.get(5),
                    failed: r.get(6),
                    cancelled: r.get(7),
                    created_at: at(r.get(8)),
                    finished_at: r.get::<Option<i64>, _>(9).map(at),
                    hooks: HOOKS
                        .iter()
                        .zip(fired)
                        .filter_map(|(name, fired)| {
                            let job = hooks.get(name)?.get("name")?.as_str()?.to_owned();
                            Some(((*name).to_owned(), job, fired != 0))
                        })
                        .collect(),
                }
            })
            .collect())
    }
}
