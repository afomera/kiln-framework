use kiln::{auth::SignIn, prelude::*};

#[derive(View)]
#[view(template = "sign_ins/index")]
pub struct Index {
    pub sign_ins: Vec<SignIn>,
}
