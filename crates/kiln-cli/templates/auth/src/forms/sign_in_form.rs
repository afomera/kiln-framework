use kiln::prelude::*;

#[derive(Form)]
pub struct SignInForm {
    #[validate(email)]
    pub email: String,
    #[form(password, autocomplete = "current-password")]
    #[validate(length(min = 1))]
    pub password: String,
}
