// A has_many doesn't join: it would repeat each post once per comment.
use blog::records::*;
use kiln::prelude::*;

async fn commented(db: &Db) -> Result<Vec<Post>> {
    Post::query().join(post::comments).all(db).await
}

fn main() {}
