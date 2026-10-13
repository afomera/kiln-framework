use kiln::prelude::*;

use crate::{forms::CommentForm, records::*};

#[derive(View)]
#[view(template = "posts/show")]
pub struct Show {
    pub post: Loaded<Post, (post::author, post::editor, post::comments<(comment::author,)>)>,
    pub by_line: String,
    pub can_edit: bool,
    pub comment_form: FormState<CommentForm>,
}
