use kiln::{policy::Update, prelude::*};

use crate::{forms::PostForm, records::Post};

pub async fn update_post(ctx: &Ctx, post: Authorized<Update, Post>, input: Valid<PostForm>) -> Result<Post> {
    input.into_inner().apply(&post, Post::update(post.id)).run(&ctx.db).await
}
