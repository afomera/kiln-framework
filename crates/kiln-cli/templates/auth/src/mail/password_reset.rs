use kiln::{prelude::*, time::SignedDuration};
use serde::Serialize;

use crate::records::User;

/// A password reset link, good for one hour and one use.
#[derive(Mail, Serialize)]
#[mail(template = "mail/password_reset", subject = "Reset your password", preview)]
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

/// Previewed with the first user (`kiln db seed` creates some), or a made-up one.
impl Preview for PasswordReset {
    async fn preview(ctx: &Ctx) -> Result<Self> {
        Ok(match User::query().one(&ctx.db).await? {
            Some(user) => Self::for_user(ctx, &user),
            None => Self { to: "ada@example.com".into(), name: "Ada Lovelace".into(), token: "preview".into() },
        })
    }
}
