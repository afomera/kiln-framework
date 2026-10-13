use kiln::{policy::Update, prelude::*};

use crate::records::*;

pub async fn archive_post(ctx: &Ctx, post: Authorized<Update, Post>) -> Result<Post> {
    Post::update(post.id).set(post::status, PostStatus::Archived).run(&ctx.db).await
}
