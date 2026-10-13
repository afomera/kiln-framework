use kiln::prelude::*;

use crate::{forms::SignUpForm, ops, views::registrations::New};

#[action(POST "/sign_up")]
pub async fn create(ctx: Ctx, session: Session, form: Form<SignUpForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| New { form })?;
    let state = FormState::from_input(&*input);
    let Some(user) = ops::sign_up(&ctx, input).await? else {
        return Err(Error::unprocessable(&New { form: state.with_error("email", "is already registered") }));
    };
    session.sign_in(&ctx.db, &user).await?;
    Ok(Redirect::to("/").notice("Welcome! We've emailed you a link to confirm your address."))
}
