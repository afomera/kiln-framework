// `#[action]` generates a `path(..)` URL helper, so the action can't be called `path`.
use kiln::prelude::*;

#[action(GET "/files/{*path}")]
pub async fn path(path: Param<String>) -> String {
    String::new()
}

fn main() {}
