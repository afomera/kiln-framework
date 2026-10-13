use kiln::prelude::*;

#[derive(Form)]
pub struct NewPasswordForm {
    #[form(password, label = "New password", hint = "At least 8 characters.")]
    #[validate(length(min = 8, max = 200))]
    pub password: String,
}
