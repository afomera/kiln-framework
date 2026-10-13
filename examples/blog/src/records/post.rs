use kiln::prelude::*;

use super::{Comment, User};

#[derive(Record, Debug, Clone)]
#[record(table = "posts", belongs_to(author: User, editor: User), has_many(comments: Comment), parts)]
pub struct Post {
    pub id: Id<Post>,
    #[index(unique)]
    pub slug: String,
    pub title: String,
    /// Written in the rich text editor; always sanitized HTML.
    pub body: RichText,
    /// An image shown above the post.
    pub cover: Option<Attachment>,
    pub status: PostStatus,
    pub author_id: Id<User>,
    /// Last editor, if anyone other than the author has edited the post.
    pub editor_id: Option<Id<User>>,
    pub published_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostStatus {
    Draft,
    Published,
    Archived,
}

/// The realtime stream for a post's page: `{{ turbo_stream_from("post:" ~ post.id) }}`.
pub fn stream_name(post_id: Id<Post>) -> String {
    format!("post:{post_id}")
}

// Scopes are plain associated functions. Columns are in scope as `status`, `published_at`, ...
impl Post {
    pub fn published() -> Query<Post> {
        Post::query().filter(status.eq(PostStatus::Published)).order(published_at.desc())
    }
}
