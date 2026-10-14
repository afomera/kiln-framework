// `through` names a has_many on the same record.
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "teams", has_many(members: Person, through = memberships))]
pub struct Team {
    pub id: Id<Team>,
}

#[derive(Record, Debug, Clone)]
#[record(table = "people")]
pub struct Person {
    pub id: Id<Person>,
}

fn main() {}
