//! Recurring jobs. Workers run them; see `/kiln/schedules` in development.

use kiln::prelude::*;

use crate::jobs::ArchiveOldDrafts;

pub fn schedule(s: Schedule) -> Schedule {
    s.cron("0 3 * * *", ArchiveOldDrafts {})
}
