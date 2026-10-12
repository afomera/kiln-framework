//! Default records for tests: `app.create::<User>()`. In the library (not `tests/`) so
//! factories can implement kiln's `Factory` trait for the app's records.

use kiln::{
    prelude::*,
    testing::{Factory, TestApp},
};

use crate::records::*;

impl Factory for User {
    async fn build(_app: &TestApp, n: u64) -> Result<NewUser> {
        Ok(NewUser { name: format!("User {n}"), email: format!("user{n}@example.com") })
    }
}
