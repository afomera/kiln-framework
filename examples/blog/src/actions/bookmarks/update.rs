use kiln::{policy::Update, prelude::*};

use crate::{actions::bookmarks, forms::BookmarkForm, ops, records::*, views::bookmarks::Edit};

#[action(PATCH "/bookmarks/{id}")]
pub async fn update(
    ctx: Ctx,
    user: Current<User>,
    id: Param<Id<Bookmark>>,
    form: Form<BookmarkForm>,
) -> Result<Redirect> {
    let bookmark = user.authorize::<Update, _>(Bookmark::find(*id).one_or_404(&ctx.db).await?)?;
    let input = form.validate().or_render(|form| Edit { form, bookmark: (*bookmark).clone() })?;
    let bookmark = ops::update_bookmark(&ctx, bookmark, input).await?;
    Ok(Redirect::to(bookmarks::show::path(bookmark.id)).notice("Bookmark updated."))
}
