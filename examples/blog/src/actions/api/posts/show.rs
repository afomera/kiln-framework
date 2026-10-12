use kiln::prelude::*;

use crate::{records::*, views::api::PostJson};

#[action(GET "/api/posts/{slug}", pipeline = Api)]
pub async fn show(ctx: Ctx, slug: Param<String>) -> Result<Json<PostJson>> {
    let post = Post::published().filter(post::slug.eq(&*slug)).preload(post::author).one_or_404(&ctx.db).await?;
    Ok(Json(PostJson::from(&post)))
}
