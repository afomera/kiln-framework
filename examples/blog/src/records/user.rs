use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "users")]
pub struct User {
    pub id: Id<User>,
    pub name: String,
    #[index(unique)]
    pub email: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
