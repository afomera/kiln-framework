use kiln::prelude::*;

use crate::{records::User, views::sessions::New};

/// Demo sign-in by email, until the M2 auth generator adds passwords.
#[action(GET "/session/new")]
pub async fn new(ctx: Ctx) -> Result<New> {
    let emails = User::query().all(&ctx.db).await?.into_iter().map(|u| u.email).collect();
    Ok(New { form: FormState::new(), emails })
}
