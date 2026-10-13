use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "users")]
pub struct User {
    pub id: Id<User>,
    pub name: String,
    #[index(unique)]
    pub email: String,
    /// Argon2 hash from `kiln::auth::hash_password`. `#[hidden]`: never serialized.
    #[hidden]
    pub password_digest: String,
    pub confirmed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
