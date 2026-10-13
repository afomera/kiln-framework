use kiln::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{actions::posts, config::Config, mail::CommentNotification, records::*};

/// Tells a post's author someone commented: in their inbox, and by email.
#[derive(Notification, Serialize, Deserialize, Debug)]
#[notification(description = "Someone comments on your post", optional(mail, inbox))]
pub struct CommentPosted {
    pub comment_id: Id<Comment>,
}

impl Notify for CommentPosted {
    type To = User;

    async fn deliver(&self, ctx: &Ctx, to: &User, via: &mut Via) -> Result {
        let comment = Comment::find(self.comment_id).preload(comment::author).one_or_404(&ctx.db).await?;
        let post = Post::find(comment.post_id).one_or_404(&ctx.db).await?;
        let commenter = comment.author().name.clone();
        via.inbox(format!("{commenter} commented on “{}”", post.title))
            .body(comment.body.clone())
            .link(posts::show::path(&post.slug));
        // Each author can turn these off at /settings/notifications; NOTIFY_AUTHORS=false
        // turns the emails off for everyone.
        if ctx.config::<Config>().notify_authors {
            via.mail(CommentNotification { to: to.email.clone(), commenter, body: comment.body.clone(), post })?;
        }
        Ok(())
    }
}
