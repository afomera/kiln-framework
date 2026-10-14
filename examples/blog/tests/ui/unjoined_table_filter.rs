// A query only takes columns of the tables it joins: `users` is joined, `posts` isn't.
use blog::records::*;
use kiln::prelude::*;

async fn by_author_and_post_title(db: &Db) -> Result<Vec<Comment>> {
    Comment::query().join(comment::author).filter(user::name.eq("Ada")).filter(post::title.eq("Hi")).all(db).await
}

fn main() {}
