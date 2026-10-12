use kiln::prelude::*;

use crate::records::Bookmark;

#[derive(View)]
#[view(template = "bookmarks/index")]
pub struct Index {
    pub bookmarks: Vec<Bookmark>,
}
