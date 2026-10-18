use kiln::{config, prelude::*};

use crate::views::sessions::New;

/// Demo sign-in by email, until the auth generator adds passwords. Off in production.
#[action(GET "/session/new")]
pub async fn new() -> Result<New> {
    if config::is_production() {
        return Err(Error::NotFound);
    }
    Ok(New { form: FormState::new() })
}
