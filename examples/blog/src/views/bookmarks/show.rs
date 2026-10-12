use kiln::prelude::*;

use crate::records::Bookmark;

#[derive(View)]
#[view(template = "bookmarks/show")]
pub struct Show {
    pub bookmark: Bookmark,
    pub can_edit: bool,
}
