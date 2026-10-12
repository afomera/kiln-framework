use kiln::prelude::*;

use crate::{records::*, views::posts::Show};

#[action(GET "/posts/{slug}")]
pub async fn show(ctx: Ctx, slug: Param<String>) -> Result<Show> {
    let post = Post::find_by(post::slug.eq(&*slug))
        .preload(post::author)
        .preload(post::editor)
        .preload(post::comments.preload(comment::author))
        .one_or_404(&ctx.db)
        .await?;

    let by_line = format!("{} · {} comments", post.author().name, post.comments().len());
    Ok(Show { post, by_line })
}
