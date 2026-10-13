use kiln::prelude::*;

use crate::records::*;

/// Marks the email confirmed. `None` if the link is invalid or expired.
pub async fn confirm_email(ctx: &Ctx, token: &str) -> Result<Option<User>> {
    let tokens = ctx.tokens();
    let Some(id) = tokens.read::<User>("email_confirmation", token) else { return Ok(None) };
    let Some(found) = User::find(id).one(&ctx.db).await?.filter(|u| tokens.matches(token, &u.email)) else {
        return Ok(None);
    };
    if found.confirmed_at.is_some() {
        return Ok(Some(found));
    }
    let confirmed = User::update(found.id).set(user::confirmed_at, Some(Timestamp::now())).run(&ctx.db).await?;
    Ok(Some(confirmed))
}
