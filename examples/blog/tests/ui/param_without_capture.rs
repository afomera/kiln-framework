// A `Param` argument needs a matching `{capture}` in the route.
use kiln::prelude::*;

#[action(GET "/posts")]
pub async fn index(page: Param<i64>) -> String {
    String::new()
}

fn main() {}
