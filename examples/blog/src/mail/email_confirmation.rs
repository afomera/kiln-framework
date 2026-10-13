use kiln::{prelude::*, time::SignedDuration};
use serde::Serialize;

use crate::records::User;

/// Sent at sign-up. The link works for three days.
#[derive(Mail, Serialize)]
#[mail(template = "mail/email_confirmation", subject = "Confirm your email", preview)]
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

/// Previewed with the first user (`kiln db seed` creates some).
impl Preview for EmailConfirmation {
    async fn preview(ctx: &Ctx) -> Result<Self> {
        let user = User::query()
            .one(&ctx.db)
            .await?
            .ok_or_else(|| Error::BadRequest("create a user to preview this mail".into()))?;
        Ok(Self::for_user(ctx, &user))
    }
}
