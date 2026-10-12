use kiln::{policy::Create, prelude::*};

use crate::{actions::bookmarks, forms::BookmarkForm, ops, records::User, views::bookmarks::New};

#[action(POST "/bookmarks")]
pub async fn create(ctx: Ctx, user: Current<User>, form: Form<BookmarkForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| New { form })?;
    let new = user.authorize::<Create, _>(input.into_inner().into_new(&user))?;
    let bookmark = ops::create_bookmark(&ctx, new).await?;
    Ok(Redirect::to(bookmarks::show::path(bookmark.id)).notice("Bookmark created."))
}
