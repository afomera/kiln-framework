use kiln::prelude::*;

#[derive(Form)]
pub struct SignUpForm {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[validate(email)]
    pub email: String,
    #[form(password, hint = "At least 8 characters.")]
    #[validate(length(min = 8, max = 200))]
    pub password: String,
}
