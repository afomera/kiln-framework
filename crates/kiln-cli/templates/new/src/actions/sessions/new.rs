use kiln::prelude::*;

use crate::views::sessions::New;

/// Demo sign-in by email, until the auth generator adds passwords.
#[action(GET "/session/new")]
pub async fn new() -> New {
    New { form: FormState::new() }
}
