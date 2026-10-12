use kiln::prelude::*;

use crate::{records::User, views::bookmarks::New};

#[action(GET "/bookmarks/new")]
pub async fn new(_user: Current<User>) -> New {
    New { form: FormState::new() }
}
