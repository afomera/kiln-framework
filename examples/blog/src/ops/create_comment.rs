use kiln::{policy::Create, prelude::*};

use crate::{
    config::Config,
    mail::CommentNotification,
    records::{Comment, CommentRelations, NewComment, Post, PostRelations, comment, post},
    views::comments::Item,
};

/// Saves the comment, broadcasts it to everyone viewing the post, and emails the post's
/// author. The broadcast and the email are part of the transaction: if the insert rolls
/// back, nobody sees the comment.
pub async fn create_comment(
    ctx: &Ctx,
    new: Authorized<Create, NewComment>,
) -> Result<Loaded<Comment, (comment::author,)>> {
    ctx.db
        .transaction(|tx| async move {
            let comment = Comment::insert(new.into_inner()).run(&tx).await?;
            let comment = Comment::find(comment.id).preload(comment::author).one_or_404(&tx).await?;
            let item = TurboStream::new().append("comments", Item { comment: comment.clone() });
            tx.broadcast(&post::stream_name(comment.post_id), item).await?;

            let parent = Post::find(comment.post_id).preload(post::author).one_or_404(&tx).await?;
            if ctx.config::<Config>().notify_authors && parent.author_id != comment.author_id {
                CommentNotification {
                    to: parent.author().email.clone(),
                    commenter: comment.author().name.clone(),
                    body: comment.body.clone(),
                    post: (*parent).clone(),
                }
                .deliver_later(&tx)
                .await?;
            }
            Ok(comment)
        })
        .await
}
