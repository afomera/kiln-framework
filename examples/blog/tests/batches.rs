//! Job batches: hooks on success, failure, completion and cancellation.

use std::sync::atomic::{AtomicUsize, Ordering};

use kiln::{prelude::*, testing::TestApp};
use serde::{Deserialize, Serialize};

#[derive(Job, Serialize, Deserialize)]
struct Step {
    fail: bool,
}

impl Perform for Step {
    async fn perform(self, _ctx: &Ctx) -> Result {
        if self.fail { Err(Error::BadRequest("step broke".into())) } else { Ok(()) }
    }
}

#[derive(Job, Serialize, Deserialize)]
#[job(attempts = 1)]
struct OneShot {
    fail: bool,
}

impl Perform for OneShot {
    async fn perform(self, _ctx: &Ctx) -> Result {
        if self.fail { Err(Error::BadRequest("one shot broke".into())) } else { Ok(()) }
    }
}

/// Fans out: adds two more steps to its own batch.
#[derive(Job, Serialize, Deserialize)]
struct FanOut;

impl Perform for FanOut {
    async fn perform(self, ctx: &Ctx) -> Result {
        let batch = BatchId::current().expect("runs in a batch");
        batch.add(&ctx.db, Step { fail: false }).await?;
        batch.add(&ctx.db, Step { fail: false }).await?;
        Ok(())
    }
}

static HOOKS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static SEEN_SUCCEEDED: AtomicUsize = AtomicUsize::new(0);

#[derive(Job, Serialize, Deserialize)]
struct Hook {
    tag: String,
}

impl Perform for Hook {
    async fn perform(self, ctx: &Ctx) -> Result {
        // Hooks see their batch.
        let status = BatchId::current().expect("hooks know their batch").status(&ctx.db).await?;
        SEEN_SUCCEEDED.store(status.succeeded as usize, Ordering::SeqCst);
        HOOKS.lock().unwrap().push(format!("{}:{}", status.description, self.tag));
        Ok(())
    }
}

fn hooks_for(description: &str) -> Vec<String> {
    let mut fired: Vec<String> = HOOKS
        .lock()
        .unwrap()
        .iter()
        .filter_map(|h| h.strip_prefix(&format!("{description}:")).map(str::to_owned))
        .collect();
    fired.sort();
    fired
}

fn with_hooks(batch: Batch) -> Batch {
    batch
        .on_success(Hook { tag: "success".into() })
        .on_failure(Hook { tag: "failure".into() })
        .on_complete(Hook { tag: "complete".into() })
        .on_cancel(Hook { tag: "cancel".into() })
}

#[kiln::test]
async fn a_batch_that_succeeds_fires_success_and_complete(app: TestApp) -> Result {
    let batch = with_hooks(Batch::new("all good").jobs((0..3).map(|_| Step { fail: false }))).enqueue(&app.db).await?;
    assert_eq!(batch.status(&app.db).await?.pending, 3);
    app.perform_enqueued_jobs().await?;
    let status = batch.status(&app.db).await?;
    assert!(status.succeeded_entirely(), "{status:?}");
    assert_eq!((status.succeeded, status.pending), (3, 0));
    assert_eq!(hooks_for("all good"), ["complete", "success"]);
    assert!(status.hooks.iter().all(|(name, _, fired)| *fired == (name == "success" || name == "complete")));
    Ok(())
}

#[kiln::test]
async fn a_failed_job_fires_failure_once_and_complete_without_success(app: TestApp) -> Result {
    let batch = with_hooks(
        Batch::new("one bad").job(OneShot { fail: false }).job(OneShot { fail: true }).job(OneShot { fail: true }),
    )
    .enqueue(&app.db)
    .await?;
    assert!(app.perform_enqueued_jobs().await.is_err(), "two jobs failed for good");
    let status = batch.status(&app.db).await?;
    assert_eq!((status.status.as_str(), status.succeeded, status.failed), ("finished", 1, 2));
    assert_eq!(hooks_for("one bad"), ["complete", "failure"], "failure fires once, success never");
    Ok(())
}

#[kiln::test]
async fn retrying_a_failed_job_reopens_the_batch(app: TestApp) -> Result {
    let batch = Batch::new("retry").job(OneShot { fail: true }).enqueue(&app.db).await?;
    let _ = app.perform_enqueued_jobs().await;
    assert_eq!(batch.status(&app.db).await?.status, "finished");
    app.db.retry_jobs(None).await?;
    let status = batch.status(&app.db).await?;
    assert_eq!((status.status.as_str(), status.pending, status.failed), ("running", 1, 0));
    Ok(())
}

#[kiln::test]
async fn cancelling_removes_waiting_jobs_and_fires_cancel_only(app: TestApp) -> Result {
    let batch = with_hooks(Batch::new("stop it").jobs((0..4).map(|_| Step { fail: false }))).enqueue(&app.db).await?;
    assert!(batch.cancel(&app.db).await?);
    assert!(!batch.cancel(&app.db).await?, "cancelling twice does nothing");
    let status = batch.status(&app.db).await?;
    assert_eq!((status.status.as_str(), status.cancelled, status.pending), ("cancelled", 4, 0));
    app.perform_enqueued_jobs().await?;
    assert_eq!(hooks_for("stop it"), ["cancel"]);
    Ok(())
}

#[kiln::test]
async fn jobs_can_add_to_their_own_batch(app: TestApp) -> Result {
    let batch = with_hooks(Batch::new("fan out").job(FanOut)).enqueue(&app.db).await?;
    app.perform_enqueued_jobs().await?;
    let status = batch.status(&app.db).await?;
    assert_eq!((status.total, status.succeeded), (3, 3));
    assert_eq!(hooks_for("fan out"), ["complete", "success"]);
    Ok(())
}

#[kiln::test]
async fn batches_roll_back_with_their_transaction(app: TestApp) -> Result {
    let result: Result<()> = app
        .db
        .transaction(|tx| async move {
            Batch::new("never").job(Step { fail: false }).enqueue(&tx).await?;
            Err(Error::BadRequest("rolled back".into()))
        })
        .await;
    assert!(result.is_err());
    assert!(app.db.batches(10).await?.is_empty());
    assert_eq!(app.perform_enqueued_jobs().await?, 0);
    Ok(())
}

#[kiln::test]
async fn an_empty_batch_succeeds_at_once(app: TestApp) -> Result {
    let batch = with_hooks(Batch::new("empty")).enqueue(&app.db).await?;
    assert!(batch.status(&app.db).await?.succeeded_entirely());
    app.perform_enqueued_jobs().await?;
    assert_eq!(hooks_for("empty"), ["complete", "success"]);
    Ok(())
}
