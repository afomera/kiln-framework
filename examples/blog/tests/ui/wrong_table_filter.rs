// A Comment column can't filter a Post query.
use blog::records::*;
use kiln::prelude::*;

fn mismatched() -> Query<Post> {
    Post::query().filter(comment::body.eq("hi"))
}

fn main() {}
