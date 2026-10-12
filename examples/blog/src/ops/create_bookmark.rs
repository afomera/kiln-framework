use kiln::{policy::Create, prelude::*};

use crate::records::{Bookmark, NewBookmark};

pub async fn create_bookmark(ctx: &Ctx, new: Authorized<Create, NewBookmark>) -> Result<Bookmark> {
    Bookmark::insert(new.into_inner()).run(&ctx.db).await
}
