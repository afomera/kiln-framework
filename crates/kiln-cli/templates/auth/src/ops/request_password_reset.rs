use kiln::prelude::*;

use crate::{mail::PasswordReset, records::*};

/// Emails a reset link when the address has an account. The caller says the same thing
/// either way, so the form can't be used to find out who has an account.
pub async fn request_password_reset(ctx: &Ctx, email: &str) -> Result {
    if let Some(found) = User::find_by(user::email.eq(email)).one(&ctx.db).await? {
        PasswordReset::for_user(ctx, &found).deliver_later(&ctx.db).await?;
    }
    Ok(())
}
