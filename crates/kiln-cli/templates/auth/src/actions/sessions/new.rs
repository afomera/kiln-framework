use kiln::prelude::*;

use crate::views::sessions::New;

#[action(GET "/session/new")]
pub async fn new() -> New {
    New { form: FormState::new() }
}
