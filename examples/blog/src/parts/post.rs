use kiln::prelude::*;

use crate::records::{Post, PostStatus};

/// Presentation logic for posts. Templates read these as values: `{{ post.reading_time }}`.
#[part]
impl Post {
    pub fn reading_time(&self) -> String {
        let minutes = self.body.split_whitespace().count() / 230 + 1;
        format!("{minutes} min read")
    }

    pub fn is_draft(&self) -> bool {
        self.status == PostStatus::Draft
    }
}
