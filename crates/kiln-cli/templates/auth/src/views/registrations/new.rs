use kiln::prelude::*;

use crate::forms::SignUpForm;

#[derive(View)]
#[view(template = "registrations/new")]
pub struct New {
    pub form: FormState<SignUpForm>,
}
