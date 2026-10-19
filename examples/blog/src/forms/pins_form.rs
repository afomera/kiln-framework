use kiln::prelude::*;

use crate::records::Bookmark;

/// Which of your bookmarks are pinned: a checkbox per bookmark, all named `pinned`
/// (`pinned=3&pinned=7`). Unchecking every box submits an empty list.
#[derive(Form, Debug)]
pub struct PinsForm {
    #[validate(length(max = 100))]
    pub pinned: Vec<Id<Bookmark>>,
}
