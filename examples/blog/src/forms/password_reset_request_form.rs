use kiln::prelude::*;

#[derive(Form)]
pub struct PasswordResetRequestForm {
    #[validate(email)]
    pub email: String,
}
