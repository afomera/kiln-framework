use kiln::{policy::Delete, prelude::*};

use crate::{actions::bookmarks, ops, records::*};

#[action(DELETE "/bookmarks/{id}")]
pub async fn destroy(ctx: Ctx, user: Current<User>, id: Param<Id<Bookmark>>) -> Result<Redirect> {
    let bookmark = user.authorize::<Delete, _>(Bookmark::find(*id).one_or_404(&ctx.db).await?)?;
    ops::delete_bookmark(&ctx, bookmark).await?;
    Ok(Redirect::to(bookmarks::index::path()).notice("Bookmark deleted."))
}
