use kiln::{policy::Update, prelude::*};

use crate::{forms::BookmarkForm, records::Bookmark};

pub async fn update_bookmark(
    ctx: &Ctx,
    bookmark: Authorized<Update, Bookmark>,
    input: Valid<BookmarkForm>,
) -> Result<Bookmark> {
    input.into_inner().apply(Bookmark::update(bookmark.id)).run(&ctx.db).await
}
