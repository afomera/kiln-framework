use kiln::{policy::Delete, prelude::*};

use crate::records::{Comment, Post, comment};

/// Deletes the post and its comments together.
pub async fn delete_post(ctx: &Ctx, post: Authorized<Delete, Post>) -> Result {
    let id = post.id;
    ctx.db
        .transaction(|tx| async move {
            Comment::query().filter(comment::post_id.eq(id)).delete_all(&tx).await?;
            Post::delete(id).run(&tx).await
        })
        .await
}
