use kiln::prelude::*;

use crate::{forms::BookmarkFilters, records::Bookmark};

#[derive(View)]
#[view(template = "bookmarks/index")]
pub struct Index {
    pub bookmarks: Vec<Bookmark>,
    /// The filters in effect; the template builds its filter links from them.
    pub filters: QueryParams<BookmarkFilters>,
}
