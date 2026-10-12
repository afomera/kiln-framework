// Using a relation that wasn't preloaded is a compile error, not a lazy query.
use blog::records::*;
use kiln::prelude::*;

fn comment_counts(posts: &[Loaded<Post, (post::author,)>]) -> Vec<usize> {
    posts.iter().map(|p| p.comments().len()).collect()
}

fn main() {}
