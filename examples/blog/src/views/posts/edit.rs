use kiln::prelude::*;

use crate::{forms::PostForm, records::Post};

#[derive(View)]
#[view(template = "posts/edit")]
pub struct Edit {
    pub form: FormState<PostForm>,
    pub post: Post,
}
