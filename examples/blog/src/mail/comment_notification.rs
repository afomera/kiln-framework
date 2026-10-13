use kiln::prelude::*;
use serde::Serialize;

use crate::records::{Post, PostStatus};

/// Tells a post's author someone commented.
#[derive(Mail, Serialize, Debug)]
#[mail(template = "mail/comment_notification", subject = "{{ commenter }} commented on “{{ post.title }}”", preview)]
pub struct CommentNotification {
    pub to: String,
    pub post: Post,
    pub commenter: String,
    pub body: String,
}

impl Preview for CommentNotification {
    async fn preview(_ctx: &Ctx) -> Result<Self> {
        let post = Post {
            id: Id::new(1),
            slug: "hello-kiln".into(),
            title: "Hello, Kiln".into(),
            body: "The compiler is the reviewer.".into(),
            cover: None,
            status: PostStatus::Published,
            author_id: Id::new(1),
            editor_id: None,
            published_at: Some(Timestamp::now()),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        Ok(Self {
            to: "ada@example.com".into(),
            post,
            commenter: "Grace Hopper".into(),
            body: "Typed paths are lovely.".into(),
        })
    }
}
