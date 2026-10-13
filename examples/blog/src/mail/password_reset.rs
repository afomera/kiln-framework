use kiln::{prelude::*, time::SignedDuration};
use serde::Serialize;

use crate::records::User;

/// A password reset link, good for one hour and one use.
#[derive(Mail, Serialize)]
#[mail(template = "mail/password_reset", subject = "Reset your password")]
pub struct PasswordReset {
    pub to: String,
    pub name: String,
    pub token: String,
}

impl PasswordReset {
    pub fn for_user(ctx: &Ctx, user: &User) -> Self {
        // Bound to the current password: once it changes, the link stops working.
        let token = ctx.tokens().make("password_reset", user.id, &user.password_digest, SignedDuration::from_hours(1));
        Self { to: user.email.clone(), name: user.name.clone(), token }
    }
}
