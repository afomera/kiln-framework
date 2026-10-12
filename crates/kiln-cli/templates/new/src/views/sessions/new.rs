use kiln::prelude::*;

use crate::forms::SignInForm;

#[derive(View)]
#[view(template = "sessions/new")]
pub struct New {
    pub form: FormState<SignInForm>,
}
