use kiln::prelude::*;

use crate::records::{Comment, comment};

/// One comment: rendered in the post page, in stream responses and in broadcasts.
#[derive(View)]
#[view(template = "comments/_comment", layout = none)]
pub struct Item {
    pub comment: Loaded<Comment, (comment::author,)>,
}
