// A view pointing at a template file that doesn't exist fails at compile time.
use kiln::prelude::*;

#[derive(View)]
#[view(template = "posts/missing")]
pub struct Missing {
    pub title: String,
}

fn main() {}
