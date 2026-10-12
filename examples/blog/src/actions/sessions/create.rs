use kiln::prelude::*;

use crate::{actions::posts, forms::SignInForm, records::*, views::sessions::New};

#[action(POST "/session")]
pub async fn create(ctx: Ctx, session: Session, form: Form<SignInForm>) -> Result<Redirect> {
    let emails: Vec<String> = User::query().all(&ctx.db).await?.into_iter().map(|u| u.email).collect();
    let input = form.validate().or_render(|form| New { form, emails: emails.clone() })?;
    let Some(user) = User::find_by(user::email.eq(&input.email)).one(&ctx.db).await? else {
        let form = FormState::from_input(&*input).with_error("email", "doesn't match any user");
        return Err(Error::unprocessable(&New { form, emails }));
    };
    session.sign_in(&user);
    Ok(Redirect::to(posts::index::path()).notice(format!("Signed in as {}.", user.name)))
}
