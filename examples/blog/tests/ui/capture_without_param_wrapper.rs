// A route capture is read with `Param<T>`, not as a bare `T`.
use kiln::prelude::*;

#[action(GET "/posts/{slug}")]
pub async fn show(ctx: Ctx, slug: String) -> String {
    slug
}

fn main() {}
