//! On SIGTERM, workers stop claiming and release jobs that outlive the grace period back to
//! the queue. Its own binary: a shutdown is process-wide.

use std::time::Duration;

use kiln::{prelude::*, testing::TestApp};
use serde::{Deserialize, Serialize};

#[derive(Job, Serialize, Deserialize)]
struct Lengthy;

impl Perform for Lengthy {
    async fn perform(self, _ctx: &Ctx) -> Result {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }
}

#[kiln::test]
async fn shutdown_releases_running_jobs(app: TestApp) -> Result {
    unsafe { std::env::set_var("JOB_SHUTDOWN_GRACE", "1") };
    Lengthy.enqueue(&app.db).await?;
    let pool = tokio::spawn(kiln::jobs::work(app.app().ctx().clone(), 1));
    for _ in 0..100 {
        if app.db.job_counts().await? == vec![("running".to_owned(), 1)] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    kiln::jobs::begin_shutdown();
    tokio::time::timeout(Duration::from_secs(5), pool).await.expect("workers stop after the grace period").ok();
    assert_eq!(app.db.job_counts().await?, vec![("queued".to_owned(), 1)], "released, not lost");
    app.assert_enqueued::<Lengthy>(1).await?;
    let attempts = kiln::testing::job_attempts(&app.db, "Lengthy").await?;
    assert_eq!(attempts, [0], "a released attempt doesn't count");
    Ok(())
}
