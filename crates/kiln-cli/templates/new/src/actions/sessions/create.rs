use kiln::{config, prelude::*};

use crate::{forms::SignInForm, records::*, views::sessions::New};

/// Demo sign-in by email, without a password: off in production (a 404) until `kiln g auth`
/// replaces it, since anyone who knows an email could sign in as that person.
#[action(POST "/session")]
pub async fn create(ctx: Ctx, session: Session, form: Form<SignInForm>) -> Result<Redirect> {
    if config::is_production() {
        return Err(Error::NotFound);
    }
    let input = form.validate().or_render(|form| New { form })?;
    let Some(user) = User::find_by(user::email.eq(&input.email)).one(&ctx.db).await? else {
        // Doesn't say whether an account has the email.
        let form = FormState::from_input(&*input).with_error("email", "can't be used to sign in");
        return Err(Error::unprocessable(&New { form }));
    };
    session.sign_in(&ctx.db, &user).await?;
    Ok(Redirect::to("/").notice(format!("Signed in as {}.", user.name)))
}
