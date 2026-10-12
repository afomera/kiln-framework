use kiln::prelude::*;

use crate::{records::*, views::bookmarks::Index};

/// The signed-in user's bookmarks only.
#[action(GET "/bookmarks")]
pub async fn index(ctx: Ctx, user: Current<User>) -> Result<Index> {
    let bookmarks = Bookmark::owned_by(&user).all(&ctx.db).await?;
    Ok(Index { bookmarks })
}
