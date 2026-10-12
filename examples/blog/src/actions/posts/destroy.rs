use kiln::{policy::Delete, prelude::*};

use crate::{actions::posts, ops, records::*};

#[action(DELETE "/posts/{slug}")]
pub async fn destroy(ctx: Ctx, user: Current<User>, slug: Param<String>) -> Result<Redirect> {
    let post = Post::find_by(post::slug.eq(&*slug)).one_or_404(&ctx.db).await?;
    let post = user.authorize::<Delete, _>(post)?;
    ops::delete_post(&ctx, post).await?;
    Ok(Redirect::to(posts::index::path()).notice("Post deleted."))
}
