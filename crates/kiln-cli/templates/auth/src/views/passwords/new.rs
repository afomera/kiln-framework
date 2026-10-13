use kiln::prelude::*;

use crate::forms::PasswordResetRequestForm;

#[derive(View)]
#[view(template = "passwords/new")]
pub struct New {
    pub form: FormState<PasswordResetRequestForm>,
}
