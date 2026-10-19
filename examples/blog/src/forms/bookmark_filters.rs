use kiln::prelude::*;

use crate::records::{Bookmark, User, bookmark};

/// The bookmarks list's query string: `/bookmarks?q=rust&sort=rating&pinned=true&rating=4&rating=5`.
/// Read with `QueryParams<BookmarkFilters>`; missing or unreadable values are the defaults.
#[derive(Form, Debug, Clone, Default)]
pub struct BookmarkFilters {
    /// Matched against titles.
    #[validate(length(max = 100))]
    pub q: Option<String>,
    pub sort: BookmarkSort,
    pub pinned: bool,
    /// Any of these ratings (`?rating=4&rating=5`); empty for all.
    pub rating: Vec<i64>,
}

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Default)]
pub enum BookmarkSort {
    #[default]
    Newest,
    Title,
    Rating,
}

impl BookmarkFilters {
    /// `who`'s bookmarks, narrowed and ordered by these filters.
    pub fn apply(&self, who: &User) -> Query<Bookmark> {
        let mut query = Bookmark::query().filter(bookmark::owner_id.eq(who.id));
        if let Some(q) = &self.q {
            // Escaped, so a `%` or `_` someone types matches only itself.
            query = query.filter(bookmark::title.ilike(kiln::like::contains(q)));
        }
        if self.pinned {
            query = query.filter(bookmark::pinned.eq(true));
        }
        if !self.rating.is_empty() {
            query = query.filter(bookmark::rating.is_in(&self.rating));
        }
        match self.sort {
            BookmarkSort::Newest => query.order(bookmark::created_at.desc()),
            BookmarkSort::Title => query.order(bookmark::title.asc()),
            BookmarkSort::Rating => query.order(bookmark::rating.desc()).order(bookmark::created_at.desc()),
        }
    }
}
