use kiln::prelude::*;

use crate::forms::BookmarkForm;

#[derive(View)]
#[view(template = "bookmarks/new")]
pub struct New {
    pub form: FormState<BookmarkForm>,
}
