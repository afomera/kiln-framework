use kiln::prelude::*;

use super::{Post, User};

#[derive(Record, Debug, Clone)]
#[record(table = "comments", belongs_to(post: Post, author: User))]
pub struct Comment {
    pub id: Id<Comment>,
    /// Comments go with their post.
    #[on_delete(cascade)]
    pub post_id: Id<Post>,
    pub author_id: Id<User>,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
