// `Form<T>` reads the request body, so nothing can come after it.
use kiln::prelude::*;

#[action(POST "/posts/{slug}/comments")]
pub async fn create(slug: Param<String>, form: Form<blog::forms::CommentForm>, headers: HeaderMap) -> Result<Redirect> {
    Ok(Redirect::to("/"))
}

fn main() {}
