// A route capture without a matching `Param` argument doesn't compile.
use kiln::prelude::*;

#[action(GET "/posts/{slug}")]
pub async fn show(ctx: Ctx) -> Result<String> {
    Ok(String::new())
}

fn main() {}
