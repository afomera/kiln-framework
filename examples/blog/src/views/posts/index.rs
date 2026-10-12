use kiln::prelude::*;

use crate::records::*;

#[derive(View)]
#[view(template = "posts/index")]
pub struct Index {
    pub posts: Vec<Loaded<Post, (post::author, post::comments)>>,
}
