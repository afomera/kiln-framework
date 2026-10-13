use kiln::prelude::*;

use crate::forms::CommentForm;

/// The comment form, inside its `new_comment` Turbo Frame.
#[derive(View)]
#[view(template = "comments/_form", layout = none)]
pub struct Compose {
    pub form: FormState<CommentForm>,
    pub post_slug: String,
}
