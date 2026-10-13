use kiln::prelude::*;

use crate::{actions::sessions, forms::PasswordResetRequestForm, ops, views::passwords::New};

#[action(POST "/passwords")]
pub async fn create(ctx: Ctx, form: Form<PasswordResetRequestForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| New { form })?;
    ops::request_password_reset(&ctx, &input.email).await?;
    Ok(Redirect::to(sessions::new::path()).notice("If that email has an account, a reset link is on its way."))
}
