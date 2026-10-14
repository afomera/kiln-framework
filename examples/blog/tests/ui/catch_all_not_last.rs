// A catch-all takes the rest of the path, so nothing can follow it.
use kiln::prelude::*;

#[action(GET "/{owner}/{repo}/blob/{*path}/edit")]
pub async fn edit(owner: Param<String>, repo: Param<String>, path: Param<String>) -> String {
    String::new()
}

fn main() {}
