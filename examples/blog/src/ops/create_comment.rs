use kiln::{policy::Create, prelude::*};

use crate::{
    notifications::CommentPosted,
    records::{Comment, NewComment, Post, PostRelations, comment, post},
    views::comments::Item,
};

/// Saves the comment, broadcasts it to everyone viewing the post, and notifies the post's
/// author. The broadcast and the notification are part of the transaction: if the insert
/// rolls back, nobody sees the comment.
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
            if parent.author_id != comment.author_id {
                CommentPosted { comment_id: comment.id }.notify(&tx, parent.author()).await?;
            }
            Ok(comment)
        })
        .await
}
