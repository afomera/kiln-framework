use kiln::policy::{Allows, Create, Delete, Read, Update};

use crate::records::{Bookmark, NewBookmark, User};

/// Bookmarks are private: only the owner sees them.
impl Allows<Read, Bookmark> for User {
    fn allows(&self, bookmark: &Bookmark) -> bool {
        bookmark.owner_id == self.id
    }
}

/// Users create bookmarks they own.
impl Allows<Create, NewBookmark> for User {
    fn allows(&self, bookmark: &NewBookmark) -> bool {
        bookmark.owner_id == self.id
    }
}

/// Only the owner edits a bookmark.
impl Allows<Update, Bookmark> for User {
    fn allows(&self, bookmark: &Bookmark) -> bool {
        bookmark.owner_id == self.id
    }
}

/// Only the owner deletes a bookmark.
impl Allows<Delete, Bookmark> for User {
    fn allows(&self, bookmark: &Bookmark) -> bool {
        bookmark.owner_id == self.id
    }
}
