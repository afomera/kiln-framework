use kiln::prelude::*;
use serde::Serialize;

use crate::records::User;

/// A security notice after a password change, so a stolen session can't change it quietly.
#[derive(Mail, Serialize)]
#[mail(template = "mail/password_changed", subject = "Your password was changed", preview)]
pub struct PasswordChanged {
    pub to: String,
    pub name: String,
}

impl PasswordChanged {
    pub fn for_user(user: &User) -> Self {
        Self { to: user.email.clone(), name: user.name.clone() }
    }
}

impl Preview for PasswordChanged {
    async fn preview(_ctx: &Ctx) -> Result<Self> {
        Ok(Self { to: "ada@example.com".into(), name: "Ada".into() })
    }
}
