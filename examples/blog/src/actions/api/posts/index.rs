use kiln::prelude::*;

use crate::{records::*, views::api::PostJson};

#[action(GET "/api/posts", pipeline = Api)]
pub async fn index(ctx: Ctx) -> Result<Json<Vec<PostJson>>> {
    let posts = Post::published().preload(post::author).all(&ctx.db).await?;
    Ok(Json(posts.iter().map(PostJson::from).collect()))
}
