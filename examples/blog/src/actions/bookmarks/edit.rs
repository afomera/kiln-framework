use kiln::{policy::Update, prelude::*};

use crate::{forms::BookmarkForm, records::*, views::bookmarks::Edit};

#[action(GET "/bookmarks/{id}/edit")]
pub async fn edit(ctx: Ctx, user: Current<User>, id: Param<Id<Bookmark>>) -> Result<Edit> {
    let bookmark = user.authorize::<Update, _>(Bookmark::find(*id).one_or_404(&ctx.db).await?)?;
    Ok(Edit { form: FormState::from_input(&BookmarkForm::from_record(&bookmark)), bookmark: bookmark.into_inner() })
}
