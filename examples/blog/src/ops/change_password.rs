use kiln::{auth, prelude::*};

use crate::{forms::ChangePasswordForm, mail::PasswordChanged, records::*};

/// Why a password change was refused, for the form.
pub enum PasswordChange {
    Changed(User),
    WrongCurrentPassword,
    ConfirmationMismatch,
}

/// Changes the signed-in user's password, after checking their current one, and emails them
/// that it changed. Signing out their other devices is the caller's job (it has the session).
pub async fn change_password(ctx: &Ctx, user: &User, input: Valid<ChangePasswordForm>) -> Result<PasswordChange> {
    let input = input.into_inner();
    if !auth::check_password(&input.current_password, Some(&user.password_digest)) {
        return Ok(PasswordChange::WrongCurrentPassword);
    }
    if input.password != input.password_confirmation {
        return Ok(PasswordChange::ConfirmationMismatch);
    }
    let digest = auth::hash_password(&input.password)?;
    ctx.db
        .transaction(|tx| async move {
            // A new digest also retires every outstanding password-reset link.
            let changed = User::update(user.id).set(user::password_digest, digest).run(&tx).await?;
            PasswordChanged::for_user(&changed).deliver_later(&tx).await?;
            Ok(PasswordChange::Changed(changed))
        })
        .await
}
