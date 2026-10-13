use kiln::prelude::*;

use crate::{records::User, views::password_settings::Edit};

#[action(GET "/settings/password/edit")]
pub async fn edit(_user: Current<User>) -> Edit {
    Edit { form: FormState::new() }
}
