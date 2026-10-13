use kiln::prelude::*;

#[derive(Form)]
pub struct ChangePasswordForm {
    #[form(password, label = "Current password", autocomplete = "current-password")]
    #[validate(length(min = 1))]
    pub current_password: String,
    #[form(password, label = "New password", hint = "At least 8 characters.")]
    #[validate(length(min = 8, max = 200))]
    pub password: String,
    #[form(password, label = "Confirm new password")]
    #[validate(length(min = 1))]
    pub password_confirmation: String,
}
