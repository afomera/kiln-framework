use kiln::prelude::*;

#[derive(Form, Debug)]
pub struct SignInForm {
    #[validate(email)]
    pub email: String,
}
