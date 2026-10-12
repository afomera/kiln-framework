use kiln::policy::{Allows, Create, Delete, Update};

use crate::records::{NewPost, Post, User};

/// Signed-in users write posts under their own name.
impl Allows<Create, NewPost> for User {
    fn allows(&self, post: &NewPost) -> bool {
        post.author_id == self.id
    }
}

/// Only the author edits a post.
impl Allows<Update, Post> for User {
    fn allows(&self, post: &Post) -> bool {
        post.author_id == self.id
    }
}

/// Only the author deletes a post.
impl Allows<Delete, Post> for User {
    fn allows(&self, post: &Post) -> bool {
        post.author_id == self.id
    }
}
