use kiln::prelude::*;
use serde::Serialize;

use crate::records::Post;

/// Tells a post's author someone commented.
#[derive(Mail, Serialize, Debug)]
#[mail(template = "mail/comment_notification", subject = "{{ commenter }} commented on “{{ post.title }}”")]
pub struct CommentNotification {
    pub to: String,
    pub post: Post,
    pub commenter: String,
    pub body: String,
}
