//! What every template sees as `current_user`. Templates never get the whole `User` record:
//! to show another field in a layout, add it here.

use serde::Serialize;

use crate::records::User;

#[derive(Serialize)]
pub struct CurrentUser {
    pub name: String,
    pub email: String,
}

impl CurrentUser {
    pub fn of(user: &User) -> Self {
        Self { name: user.name.clone(), email: user.email.clone() }
    }
}
