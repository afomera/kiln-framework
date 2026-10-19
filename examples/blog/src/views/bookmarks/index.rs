use kiln::prelude::*;

use crate::{forms::PinsForm, records::Bookmark};

#[derive(View)]
#[view(template = "bookmarks/index")]
pub struct Index {
    pub bookmarks: Vec<Bookmark>,
    /// One checkbox per bookmark, checked when it's pinned.
    pub form: FormState<PinsForm>,
}

impl Index {
    /// The page with the pin checkboxes showing what's pinned now.
    pub fn new(bookmarks: Vec<Bookmark>) -> Self {
        let pinned = bookmarks.iter().filter(|b| b.pinned).map(|b| b.id).collect();
        Self { form: FormState::from_input(&PinsForm { pinned }), bookmarks }
    }
}
