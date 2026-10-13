use kiln::{auth, prelude::*};

use crate::{forms::SignUpForm, mail::EmailConfirmation, records::*};

/// Creates the account and emails a confirmation link. `None` when the email is taken.
pub async fn sign_up(ctx: &Ctx, input: Valid<SignUpForm>) -> Result<Option<User>> {
    let input = input.into_inner();
    if User::find_by(user::email.eq(&input.email)).exists(&ctx.db).await? {
        return Ok(None);
    }
    let password_digest = auth::hash_password(&input.password)?;
    ctx.db
        .transaction(|tx| async move {
            let new = NewUser { name: input.name, email: input.email, password_digest, confirmed_at: None };
            let user = User::insert(new).run(&tx).await?;
            EmailConfirmation::for_user(ctx, &user).deliver_later(&tx).await?;
            Ok(Some(user))
        })
        .await
}
