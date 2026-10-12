use kiln::prelude::*;

use crate::forms::PostForm;

#[derive(View)]
#[view(template = "posts/new")]
pub struct New {
    pub form: FormState<PostForm>,
}
