use kiln::prelude::*;

use crate::{forms::BookmarkForm, records::Bookmark};

#[derive(View)]
#[view(template = "bookmarks/edit")]
pub struct Edit {
    pub form: FormState<BookmarkForm>,
    pub bookmark: Bookmark,
}
