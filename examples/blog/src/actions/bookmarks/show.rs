use kiln::{
    policy::{Read, Update},
    prelude::*,
};

use crate::{records::*, views::bookmarks::Show};

#[action(GET "/bookmarks/{id}")]
pub async fn show(ctx: Ctx, user: Current<User>, id: Param<Id<Bookmark>>) -> Result<Show> {
    let bookmark = user.authorize::<Read, _>(Bookmark::find(*id).one_or_404(&ctx.db).await?)?;
    let can_edit = user.can::<Update, _>(&*bookmark);
    Ok(Show { bookmark: bookmark.into_inner(), can_edit })
}
