// Table-level options name real fields, and `one_of` columns are nullable.
use kiln::prelude::*;

#[derive(Record)]
#[record(table = "rsvps", unique(event_id, usr_id))]
pub struct Rsvp {
    pub id: Id<Rsvp>,
    pub event_id: i64,
    pub user_id: i64,
}

#[derive(Record)]
#[record(table = "reactions", one_of(post_id, comment_id))]
pub struct Reaction {
    pub id: Id<Reaction>,
    pub post_id: i64,
    pub comment_id: Option<i64>,
}

fn main() {}
