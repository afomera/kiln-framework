// A `#[part]` block for a record that didn't opt in with `parts` would be invisible to
// templates, so it doesn't compile.
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "notes")]
pub struct Note {
    pub id: Id<Note>,
    pub body: String,
}

#[part]
impl Note {
    pub fn excerpt(&self) -> String {
        self.body.chars().take(20).collect()
    }
}

fn main() {}
