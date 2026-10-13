//! Background jobs: transactional enqueue, retries, delays and failure.

use std::sync::atomic::{AtomicUsize, Ordering};

use blog::records::*;
use kiln::{prelude::*, testing::TestApp, time::SignedDuration};
use serde::{Deserialize, Serialize};

#[derive(Job, Serialize, Deserialize, Debug, PartialEq)]
struct Retitle {
    post_id: Id<Post>,
    title: String,
}

impl Perform for Retitle {
    async fn perform(self, ctx: &Ctx) -> Result {
        Post::update(self.post_id).set(post::title, self.title).run(&ctx.db).await?;
        Ok(())
    }
}

static FLAKY_RUNS: AtomicUsize = AtomicUsize::new(0);

#[derive(Job, Serialize, Deserialize)]
#[job(queue = "flaky", attempts = 3)]
struct AlwaysFails;

impl Perform for AlwaysFails {
    async fn perform(self, _ctx: &Ctx) -> Result {
        FLAKY_RUNS.fetch_add(1, Ordering::SeqCst);
        Err(Error::BadRequest("upstream is down".into()))
    }
}

#[derive(Job, Serialize, Deserialize)]
struct Announce {
    post_id: Id<Post>,
}

impl Perform for Announce {
    async fn perform(self, ctx: &Ctx) -> Result {
        ctx.db.broadcast(&post::stream_name(self.post_id), TurboStream::new().refresh()).await
    }
}

#[kiln::test]
async fn enqueued_jobs_run_and_are_removed(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    Retitle { post_id: post.id, title: "From a job".into() }.enqueue(&app.db).await?;
    assert_eq!(app.enqueued::<Retitle>().await?, vec![Retitle { post_id: post.id, title: "From a job".into() }]);

    assert_eq!(app.perform_enqueued_jobs().await?, 1);
    assert_eq!(Post::find(post.id).one_or_404(&app.db).await?.title, "From a job");
    app.assert_enqueued::<Retitle>(0).await?;
    assert!(app.db.job_counts().await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn jobs_enqueued_in_a_rolled_back_transaction_never_run(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let result: Result<()> = app
        .db
        .transaction(|tx| async move {
            Retitle { post_id: post.id, title: "Never".into() }.enqueue(&tx).await?;
            Err(Error::BadRequest("rolled back".into()))
        })
        .await;
    assert!(result.is_err());
    app.assert_enqueued::<Retitle>(0).await?;
    assert_eq!(app.perform_enqueued_jobs().await?, 0);
    Ok(())
}

#[kiln::test]
async fn failing_jobs_retry_then_fail_for_good(app: TestApp) -> Result {
    let before = FLAKY_RUNS.load(Ordering::SeqCst);
    AlwaysFails.enqueue(&app.db).await?;
    let error = app.perform_enqueued_jobs().await.unwrap_err().to_string();
    assert!(error.contains("AlwaysFails: bad request: upstream is down"), "{error}");
    assert_eq!(FLAKY_RUNS.load(Ordering::SeqCst) - before, 3, "runs once per attempt");
    assert_eq!(app.db.job_counts().await?, vec![("failed".to_owned(), 1)]);
    let failed = app.db.failed_jobs().await?;
    assert_eq!(failed[0].1, "AlwaysFails");
    assert!(failed[0].3.contains("upstream is down"), "{failed:?}");

    assert_eq!(app.db.retry_jobs(None).await?, 1);
    app.assert_enqueued::<AlwaysFails>(1).await?;
    Ok(())
}

#[kiln::test]
async fn delayed_jobs_wait_unless_tests_flush_them(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    Retitle { post_id: post.id, title: "Later".into() }.enqueue_in(&app.db, SignedDuration::from_hours(1)).await?;
    app.assert_enqueued::<Retitle>(1).await?;
    assert_eq!(app.perform_enqueued_jobs().await?, 1, "tests run delayed jobs too");
    assert_eq!(Post::find(post.id).one_or_404(&app.db).await?.title, "Later");
    Ok(())
}

#[kiln::test]
async fn jobs_can_broadcast(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    Announce { post_id: post.id }.enqueue(&app.db).await?;
    app.perform_enqueued_jobs().await?;
    app.assert_broadcast(&post::stream_name(post.id), "action=\"refresh\"").await?;
    Ok(())
}
