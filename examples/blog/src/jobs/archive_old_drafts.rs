use kiln::{
    policy::{self, Update},
    prelude::*,
    time::SignedDuration,
};
use serde::{Deserialize, Serialize};

use crate::{ops, records::*};

/// Archives drafts nobody has touched in 90 days. Scheduled nightly in src/schedule.rs.
#[derive(Job, Serialize, Deserialize, Debug)]
#[job(queue = "maintenance", unique_for = "1h")]
pub struct ArchiveOldDrafts {}

impl Perform for ArchiveOldDrafts {
    async fn perform(self, ctx: &Ctx) -> Result {
        let cutoff = Timestamp::now() - SignedDuration::from_hours(90 * 24);
        let stale = Post::query()
            .filter(post::status.eq(PostStatus::Draft).and(post::updated_at.lt(cutoff)))
            .all(&ctx.db)
            .await?;
        for draft in stale {
            // Nobody is signed in here: the job acts as the system.
            ops::archive_post(ctx, policy::as_system::<Update, _>(draft)).await?;
        }
        Ok(())
    }
}
