use kiln::prelude::*;

use crate::forms::ChangePasswordForm;

#[derive(View)]
#[view(template = "password_settings/edit")]
pub struct Edit {
    pub form: FormState<ChangePasswordForm>,
}
