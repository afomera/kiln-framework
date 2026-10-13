//! Job priorities, queue concurrency limits, time limits and cancelling running jobs, with
//! real worker loops.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use kiln::{App, prelude::*, testing::TestApp};
use serde::{Deserialize, Serialize};

static ORDER: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Job, Serialize, Deserialize)]
#[job(queue = "ordered")]
struct Routine {
    tag: String,
}

impl Perform for Routine {
    async fn perform(self, _ctx: &Ctx) -> Result {
        ORDER.lock().unwrap().push(self.tag);
        Ok(())
    }
}

#[derive(Job, Serialize, Deserialize)]
#[job(queue = "ordered", priority = 10)]
struct Urgent {
    tag: String,
}

impl Perform for Urgent {
    async fn perform(self, _ctx: &Ctx) -> Result {
        ORDER.lock().unwrap().push(self.tag);
        Ok(())
    }
}

static RUNNING: AtomicUsize = AtomicUsize::new(0);
static MOST: AtomicUsize = AtomicUsize::new(0);

#[derive(Job, Serialize, Deserialize)]
#[job(queue = "imports")]
struct Import;

impl Perform for Import {
    async fn perform(self, _ctx: &Ctx) -> Result {
        let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
        MOST.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Job, Serialize, Deserialize)]
#[job(attempts = 1, timeout = "1s")]
struct Slow;

impl Perform for Slow {
    async fn perform(self, _ctx: &Ctx) -> Result {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }
}

static STARTED: AtomicUsize = AtomicUsize::new(0);

#[derive(Job, Serialize, Deserialize)]
#[job(queue = "long")]
struct LongRunning;

impl Perform for LongRunning {
    async fn perform(self, _ctx: &Ctx) -> Result {
        STARTED.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }
}

async fn wait_until(mut done: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if done().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting");
}

#[kiln::test]
async fn higher_priority_jobs_run_first(app: TestApp) -> Result {
    Routine { tag: "a".into() }.enqueue(&app.db).await?;
    Routine { tag: "b".into() }.enqueue(&app.db).await?;
    Urgent { tag: "urgent".into() }.enqueue(&app.db).await?;
    app.perform_enqueued_jobs().await?;
    assert_eq!(*ORDER.lock().unwrap(), ["urgent", "a", "b"]);
    Ok(())
}

#[tokio::test]
async fn queue_limits_hold_across_workers() -> Result {
    let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations");
    let app =
        TestApp::configured(&migrations, |app: App| Ok(blog::app::configure(app)?.queue_limit("imports", 2))).await?;
    for _ in 0..6 {
        Import.enqueue(&app.db).await?;
    }
    // Two separate pools of four workers, like two processes.
    let pools = [
        tokio::spawn(kiln::jobs::work(app.app().ctx().clone(), 4)),
        tokio::spawn(kiln::jobs::work(app.app().ctx().clone(), 4)),
    ];
    wait_until(async || app.db.job_counts().await.unwrap().is_empty()).await;
    pools.iter().for_each(|p| p.abort());
    assert_eq!(MOST.load(Ordering::SeqCst), 2, "never more than the limit at once");
    Ok(())
}

#[kiln::test]
async fn attempts_past_their_time_limit_fail(app: TestApp) -> Result {
    Slow.enqueue(&app.db).await?;
    let started = std::time::Instant::now();
    let error = app.perform_enqueued_jobs().await.unwrap_err().to_string();
    assert!(error.contains("Slow: timed out after 1s"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[kiln::test]
async fn running_jobs_can_be_cancelled(app: TestApp) -> Result {
    let batch = Batch::new("long").job(LongRunning).enqueue(&app.db).await?;
    let pool = tokio::spawn(kiln::jobs::work(app.app().ctx().clone(), 1));
    wait_until(async || STARTED.load(Ordering::SeqCst) == 1).await;
    let running = app.db.job_counts().await?;
    assert_eq!(running, vec![("running".to_owned(), 1)]);

    let job_id = kiln::testing::job_ids(&app.db, "LongRunning").await?[0];
    assert!(app.db.cancel_job(job_id).await?);
    wait_until(async || app.db.job_counts().await.unwrap().is_empty()).await;
    pool.abort();
    let status = batch.status(&app.db).await?;
    assert_eq!((status.status.as_str(), status.cancelled), ("finished", 1));
    Ok(())
}
