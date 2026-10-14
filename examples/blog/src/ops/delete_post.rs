use kiln::{policy::Delete, prelude::*};

use crate::records::Post;

/// Deletes the post; its comments go with it (`#[on_delete(cascade)]`).
pub async fn delete_post(ctx: &Ctx, post: Authorized<Delete, Post>) -> Result {
    Post::delete(post.id).run(&ctx.db).await
}
