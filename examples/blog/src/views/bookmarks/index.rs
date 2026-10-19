use kiln::prelude::*;

use crate::{
    forms::{BookmarkFilters, PinsForm},
    records::Bookmark,
};

#[derive(View)]
#[view(template = "bookmarks/index")]
pub struct Index {
    pub bookmarks: Vec<Bookmark>,
    /// The filters in effect; the template builds its filter links from them.
    pub filters: QueryParams<BookmarkFilters>,
    /// One checkbox per bookmark shown, checked when it's pinned.
    pub form: FormState<PinsForm>,
}

impl Index {
    /// The page with the pin checkboxes showing what's pinned now.
    pub fn new(bookmarks: Vec<Bookmark>, filters: QueryParams<BookmarkFilters>) -> Self {
        let pinned = bookmarks.iter().filter(|b| b.pinned).map(|b| b.id).collect();
        Self { form: FormState::from_input(&PinsForm { pinned }), bookmarks, filters }
    }
}
