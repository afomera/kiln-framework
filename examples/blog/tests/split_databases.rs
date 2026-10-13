//! Kiln's queue, cache and broadcasts in databases of their own (`QUEUE_DATABASE_URL` and
//! friends): what changes when a write's transaction can't include them.

use kiln::{Batch, Db, Role, prelude::*, time::SignedDuration};
use serde::{Deserialize, Serialize};

#[derive(Job, Serialize, Deserialize, Debug)]
struct Ping;

impl Perform for Ping {
    async fn perform(self, _ctx: &Ctx) -> Result {
        Ok(())
    }
}

async fn split() -> Result<(Db, Db)> {
    let queue = Db::memory().await?;
    let db = Db::memory().await?.with_role(Role::Queue, queue.clone()).with_role(Role::Streams, Db::memory().await?);
    Ok((db, queue))
}

fn count(counts: &[(String, i64)], status: &str) -> i64 {
    counts.iter().find(|(s, _)| s == status).map_or(0, |(_, n)| *n)
}

#[tokio::test]
async fn jobs_enqueued_in_a_transaction_wait_for_its_commit() -> Result {
    let (db, queue) = split().await?;
    assert!(db.is_split(Role::Queue));
    let id = db
        .transaction(|tx| async move {
            let id = Ping.enqueue(&tx).await?;
            // Held back while the write isn't committed: no worker can take it yet.
            assert_eq!(count(&tx.job_counts().await?, "pending"), 1);
            assert_eq!(count(&tx.job_counts().await?, "queued"), 0);
            Ok(id)
        })
        .await?;
    assert!(id > 0, "the job's id comes back even though it's in another database");
    let counts = queue.job_counts().await?;
    assert_eq!((count(&counts, "queued"), count(&counts, "pending")), (1, 0), "{counts:?}");
    Ok(())
}

#[tokio::test]
async fn a_rolled_back_write_enqueues_nothing() -> Result {
    let (db, queue) = split().await?;
    let result: Result<()> = db
        .transaction(|tx| async move {
            Ping.enqueue(&tx).await?;
            Batch::new("Never").job(Ping).job(Ping).enqueue(&tx).await?;
            Err(Error::BadRequest("changed my mind".into()))
        })
        .await;
    assert!(result.is_err());
    assert!(queue.job_counts().await?.is_empty(), "{:?}", queue.job_counts().await?);
    assert!(queue.batches(10).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn batches_are_released_whole_when_the_write_commits() -> Result {
    let (db, queue) = split().await?;
    let batch = db.transaction(|tx| async move { Batch::new("Import").job(Ping).job(Ping).enqueue(&tx).await }).await?;
    let status = queue.batch(batch).await?.expect("the batch");
    assert_eq!((status.status.as_str(), status.total), ("running", 2));
    assert_eq!(count(&queue.job_counts().await?, "queued"), 2);
    // An empty batch still succeeds at once, after the commit.
    let empty = db.transaction(|tx| async move { Batch::new("Nothing").enqueue(&tx).await }).await?;
    assert_eq!(queue.batch(empty).await?.expect("the batch").status, "finished");
    Ok(())
}

#[tokio::test]
async fn outside_a_transaction_nothing_waits() -> Result {
    let (db, queue) = split().await?;
    Ping.enqueue(&db).await?;
    assert_eq!(count(&queue.job_counts().await?, "queued"), 1);
    // And the app's own database never sees Kiln's queue rows.
    assert!(db.role(Role::Queue).job_counts().await?.len() == 1);
    Ok(())
}

#[tokio::test]
async fn broadcasts_follow_the_commit_too() -> Result {
    let (db, _) = split().await?;
    let streams = db.role(Role::Streams);
    let _: Result<()> = db
        .transaction(|tx| async move {
            tx.broadcast("post:1", TurboStream::new().refresh()).await?;
            Err(Error::BadRequest("rolled back".into()))
        })
        .await;
    assert!(streams.stream_messages("post:1", 0).await?.is_empty(), "a rolled-back write never broadcasts");
    db.transaction(|tx| async move { tx.broadcast("post:1", TurboStream::new().refresh()).await }).await?;
    assert_eq!(streams.stream_messages("post:1", 0).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_cache_lives_where_its_role_says() -> Result {
    let cache_db = Db::memory().await?;
    let db = Db::memory().await?.with_role(Role::Cache, cache_db.clone());
    let ctx = kiln::App::new(db.clone()).ctx().clone();
    ctx.cache().set("k", &1, SignedDuration::from_mins(1)).await?;
    assert_eq!(cache_db.cache_entries("", 10).await?.len(), 1);
    assert!(Db::memory().await?.cache_entries("", 10).await?.is_empty());
    Ok(())
}
