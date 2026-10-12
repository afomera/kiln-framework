// A part method can't share a name with a column; templates would see only one of them.
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "notes", parts)]
pub struct Note {
    pub id: Id<Note>,
    pub body: String,
}

#[part]
impl Note {
    pub fn body(&self) -> String {
        self.body.to_uppercase()
    }
}

fn main() {}
