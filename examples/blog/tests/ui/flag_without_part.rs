// A record that opts into parts must have a `#[part]` block.
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "notes", parts)]
pub struct Note {
    pub id: Id<Note>,
    pub body: String,
}

fn main() {}
