use kiln::prelude::*;

use crate::{records::User, views::posts::New};

#[action(GET "/posts/new")]
pub async fn new(_user: Current<User>) -> New {
    New { form: FormState::new() }
}
