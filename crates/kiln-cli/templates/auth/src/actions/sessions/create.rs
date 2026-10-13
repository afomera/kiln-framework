use kiln::{auth, prelude::*};

use crate::{forms::SignInForm, records::*, views::sessions::New};

#[action(POST "/session")]
pub async fn create(ctx: Ctx, session: Session, form: Form<SignInForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| New { form })?;
    let found = User::find_by(user::email.eq(&input.email)).one(&ctx.db).await?;
    // Checked even when no account has the email, so timing doesn't reveal which do.
    let correct = auth::check_password(&input.password, found.as_ref().map(|u| u.password_digest.as_str()));
    let Some(user) = found.filter(|_| correct) else {
        let form = FormState::from_input(&*input).with_error("email", "or password is incorrect");
        return Err(Error::unprocessable(&New { form }));
    };
    session.sign_in(&ctx.db, &user).await?;
    Ok(Redirect::to("/").notice(format!("Signed in as {}.", user.name)))
}
