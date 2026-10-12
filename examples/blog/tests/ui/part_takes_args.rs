// Part methods are values for templates, so they take only `&self`.
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "notes", parts)]
pub struct Note {
    pub id: Id<Note>,
    pub body: String,
}

#[part]
impl Note {
    pub fn excerpt(&self, len: usize) -> String {
        self.body.chars().take(len).collect()
    }
}

fn main() {}
