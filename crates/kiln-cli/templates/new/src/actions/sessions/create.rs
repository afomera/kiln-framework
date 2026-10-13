use kiln::prelude::*;

use crate::{forms::SignInForm, records::*, views::sessions::New};

#[action(POST "/session")]
pub async fn create(ctx: Ctx, session: Session, form: Form<SignInForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| New { form })?;
    let Some(user) = User::find_by(user::email.eq(&input.email)).one(&ctx.db).await? else {
        let form = FormState::from_input(&*input).with_error("email", "doesn't match any user");
        return Err(Error::unprocessable(&New { form }));
    };
    session.sign_in(&ctx.db, &user).await?;
    Ok(Redirect::to("/").notice(format!("Signed in as {}.", user.name)))
}
