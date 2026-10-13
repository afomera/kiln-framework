use kiln::prelude::*;

use crate::forms::NewPasswordForm;

#[derive(View)]
#[view(template = "passwords/edit")]
pub struct Edit {
    pub form: FormState<NewPasswordForm>,
    pub token: String,
}
