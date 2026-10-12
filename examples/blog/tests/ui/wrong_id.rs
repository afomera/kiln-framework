// A User's id can't be used to look up a Post.
use blog::records::*;
use kiln::prelude::*;

fn posts_for(user: &User) -> Query<Post> {
    Post::find(user.id)
}

fn main() {}
