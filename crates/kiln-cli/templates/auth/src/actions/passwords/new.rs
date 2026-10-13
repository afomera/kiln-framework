use kiln::prelude::*;

use crate::views::passwords::New;

#[action(GET "/passwords/new")]
pub async fn new() -> New {
    New { form: FormState::new() }
}
