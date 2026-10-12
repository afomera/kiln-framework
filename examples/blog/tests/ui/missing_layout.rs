// A view naming a layout that doesn't exist fails at compile time.
use kiln::prelude::*;

#[derive(View)]
#[view(template = "posts/show", layout = "admin")]
pub struct Page {
    pub title: String,
}

fn main() {}
