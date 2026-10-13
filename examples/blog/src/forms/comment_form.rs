use kiln::prelude::*;

use crate::records::{NewComment, Post, User};

#[derive(Form, Debug)]
pub struct CommentForm {
    #[validate(length(min = 1, max = 2000))]
    #[form(textarea, label = "Add a comment")]
    pub body: String,
}

impl CommentForm {
    /// The insert for a comment on `post` by `author`.
    pub fn into_new(self, post: &Post, author: &User) -> NewComment {
        NewComment { post_id: post.id, author_id: author.id, body: self.body }
    }
}
