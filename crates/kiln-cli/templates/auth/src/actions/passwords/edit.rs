use kiln::prelude::*;

use crate::{actions::passwords, ops, views::passwords::Edit};

#[action(GET "/passwords/{token}/edit")]
pub async fn edit(ctx: Ctx, token: Param<String>) -> Result<ViewOrRedirect<Edit>> {
    if ops::user_for_reset(&ctx, &token).await?.is_none() {
        let redirect = Redirect::to(passwords::new::path()).alert("That reset link is invalid or has expired.");
        return Ok(ViewOrRedirect::Redirect(redirect));
    }
    Ok(ViewOrRedirect::View(Edit { form: FormState::new(), token: token.into_inner() }))
}
