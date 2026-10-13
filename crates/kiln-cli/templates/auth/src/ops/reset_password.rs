use kiln::{auth, prelude::*};

use crate::{forms::NewPasswordForm, records::*};

/// The user a reset link is for, if the link is genuine, unexpired and unused.
pub async fn user_for_reset(ctx: &Ctx, token: &str) -> Result<Option<User>> {
    let tokens = ctx.tokens();
    let Some(id) = tokens.read::<User>("password_reset", token) else { return Ok(None) };
    Ok(User::find(id).one(&ctx.db).await?.filter(|u| tokens.matches(token, &u.password_digest)))
}

/// Sets the new password and signs the user out everywhere. `None` if the link is bad.
pub async fn reset_password(ctx: &Ctx, token: &str, input: Valid<NewPasswordForm>) -> Result<Option<User>> {
    let Some(found) = user_for_reset(ctx, token).await? else { return Ok(None) };
    let digest = auth::hash_password(&input.into_inner().password)?;
    let updated = User::update(found.id).set(user::password_digest, digest).run(&ctx.db).await?;
    auth::sign_out_everywhere(&ctx.db, &updated).await?;
    Ok(Some(updated))
}
