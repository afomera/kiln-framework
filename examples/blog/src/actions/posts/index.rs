use kiln::prelude::*;

use crate::{records::*, views::posts::Index};

#[action(GET "/posts")]
pub async fn index(ctx: Ctx) -> Result<Index> {
    let posts = Post::published().preload(post::author).preload(post::comments).all(&ctx.db).await?;
    Ok(Index { posts })
}
