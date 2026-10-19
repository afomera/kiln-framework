// Actions are `async fn`.
use kiln::prelude::*;

#[action(GET "/posts")]
pub fn index() -> String {
    String::new()
}

fn main() {}
