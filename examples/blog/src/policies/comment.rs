use kiln::policy::{Allows, Create};

use crate::records::{NewComment, User};

/// Signed-in users comment under their own name.
impl Allows<Create, NewComment> for User {
    fn allows(&self, comment: &NewComment) -> bool {
        comment.author_id == self.id
    }
}
