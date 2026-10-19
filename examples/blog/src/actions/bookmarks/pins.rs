use kiln::{policy::Update, prelude::*};

use crate::{actions::bookmarks, forms::PinsForm, ops, records::*, views::bookmarks::Index};

/// Saves the index page's pin checkboxes. Ids that aren't the user's bookmarks are a 403.
#[action(PATCH "/bookmarks/pins")]
pub async fn pins(ctx: Ctx, user: Current<User>, form: Form<PinsForm>) -> Result<Redirect> {
    let mine = Bookmark::owned_by(&user).all(&ctx.db).await?;
    let input = form.validate().or_render(|form| Index { bookmarks: mine.clone(), form })?;
    // Every checked id must be one of the user's own bookmarks.
    let checked = Bookmark::query().filter(bookmark::id.is_in(input.pinned.iter().copied())).all(&ctx.db).await?;
    for bookmark in checked {
        user.authorize::<Update, _>(bookmark)?;
    }
    let proofs = mine.into_iter().map(|b| user.authorize::<Update, _>(b)).collect::<Result<Vec<_>>>()?;
    let changed = ops::pin_bookmarks(&ctx, proofs, &input.pinned).await?;
    Ok(Redirect::to(bookmarks::index::path()).notice(format!("Pins saved ({changed} changed).")))
}
