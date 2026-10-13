use kiln::{prelude::*, time::SignedDuration};
use serde::Serialize;

use crate::records::User;

/// Sent at sign-up. The link works for three days.
#[derive(Mail, Serialize)]
#[mail(template = "mail/email_confirmation", subject = "Confirm your email")]
pub struct EmailConfirmation {
    pub to: String,
    pub name: String,
    pub token: String,
}

impl EmailConfirmation {
    pub fn for_user(ctx: &Ctx, user: &User) -> Self {
        // Bound to the address: changing the email makes old links stop working.
        let token = ctx.tokens().make("email_confirmation", user.id, &user.email, SignedDuration::from_hours(72));
        Self { to: user.email.clone(), name: user.name.clone(), token }
    }
}
