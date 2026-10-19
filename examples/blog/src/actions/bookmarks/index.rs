use kiln::prelude::*;

use crate::{forms::BookmarkFilters, records::*, views::bookmarks::Index};

/// The signed-in user's bookmarks only, narrowed by the query string
/// (`?q=..&sort=title&pinned=true&rating=5`).
#[action(GET "/bookmarks")]
pub async fn index(ctx: Ctx, user: Current<User>, filters: QueryParams<BookmarkFilters>) -> Result<Index> {
    let bookmarks = filters.apply(&user).all(&ctx.db).await?;
    Ok(Index::new(bookmarks, filters))
}
