use kiln::prelude::*;

use crate::views::registrations::New;

#[action(GET "/sign_up")]
pub async fn new() -> New {
    New { form: FormState::new() }
}
