// Action arguments are extractors (`Ctx`, `Current<User>`, `Form<T>`, ..); a record isn't one.
use blog::records::Post;
use kiln::prelude::*;

#[action(GET "/posts/latest")]
pub async fn latest(ctx: Ctx, post: Post) -> String {
    post.title
}

fn main() {}
