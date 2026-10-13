use kiln::prelude::*;

use crate::{actions::passwords, forms::NewPasswordForm, ops, views::passwords::Edit};

/// Every device is signed out by the reset; this browser is signed back in.
#[action(PATCH "/passwords/{token}")]
pub async fn update(ctx: Ctx, session: Session, token: Param<String>, form: Form<NewPasswordForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| Edit { form, token: token.to_string() })?;
    let Some(user) = ops::reset_password(&ctx, &token, input).await? else {
        return Ok(Redirect::to(passwords::new::path()).alert("That reset link is invalid or has expired."));
    };
    session.sign_in(&ctx.db, &user).await?;
    Ok(Redirect::to("/").notice("Password changed."))
}
