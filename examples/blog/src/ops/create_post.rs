use kiln::{policy::Create, prelude::*};

use crate::records::{NewPost, Post};

pub async fn create_post(ctx: &Ctx, new: Authorized<Create, NewPost>) -> Result<Post> {
    Post::insert(new.into_inner()).run(&ctx.db).await
}
