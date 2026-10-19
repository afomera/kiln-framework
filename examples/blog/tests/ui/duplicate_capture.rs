// Each capture in a route needs its own name.
use kiln::prelude::*;

#[action(GET "/{id}/compare/{id}")]
pub async fn compare(id: Param<i64>) -> String {
    String::new()
}

fn main() {}
