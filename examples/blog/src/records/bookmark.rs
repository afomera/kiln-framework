use kiln::prelude::*;

use super::User;

#[derive(Record, Debug, Clone)]
#[record(table = "bookmarks", belongs_to(owner: User))]
pub struct Bookmark {
    pub id: Id<Bookmark>,
    pub title: String,
    pub url: String,
    pub notes: Option<String>,
    pub rating: Option<i64>,
    pub pinned: bool,
    pub owner_id: Id<User>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Bookmark {
    /// The bookmarks `who` owns, newest first.
    pub fn owned_by(who: &User) -> Query<Bookmark> {
        Bookmark::query().filter(owner_id.eq(who.id)).order(created_at.desc())
    }
}
