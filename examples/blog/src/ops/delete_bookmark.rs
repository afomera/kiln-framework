use kiln::{policy::Delete, prelude::*};

use crate::records::Bookmark;

/// Records that reference this one must be deleted first; do it here, in one transaction.
pub async fn delete_bookmark(ctx: &Ctx, bookmark: Authorized<Delete, Bookmark>) -> Result {
    Bookmark::delete(bookmark.id).run(&ctx.db).await
}
