use kiln::prelude::*;

use crate::records::{Bookmark, NewBookmark, User, bookmark};

#[derive(Form, Debug)]
pub struct BookmarkForm {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    #[validate(length(min = 1, max = 200))]
    pub url: String,
    #[form(textarea)]
    pub notes: Option<String>,
    pub rating: Option<i64>,
    pub pinned: bool,
}

impl BookmarkForm {
    /// Prefills the edit form.
    pub fn from_record(bookmark: &Bookmark) -> Self {
        Self {
            title: bookmark.title.clone(),
            url: bookmark.url.clone(),
            notes: bookmark.notes.clone(),
            rating: bookmark.rating,
            pinned: bookmark.pinned,
        }
    }

    /// The insert for a new bookmark owned by `owner`.
    pub fn into_new(self, owner: &User) -> NewBookmark {
        NewBookmark {
            title: self.title,
            url: self.url,
            notes: self.notes,
            rating: self.rating,
            pinned: self.pinned,
            owner_id: owner.id,
        }
    }

    /// The update for an existing bookmark.
    pub fn apply(self, update: kiln::Update<Bookmark>) -> kiln::Update<Bookmark> {
        update
            .set(bookmark::title, self.title)
            .set(bookmark::url, self.url)
            .set(bookmark::notes, self.notes)
            .set(bookmark::rating, self.rating)
            .set(bookmark::pinned, self.pinned)
    }
}
