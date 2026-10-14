// An API route that signs someone in must say which token scope it needs.
use blog::records::User;
use kiln::prelude::*;

#[action(DELETE "/api/repos/{id}", pipeline = Api)]
pub async fn destroy(user: Current<User>, id: Param<i64>) -> Result<String> {
    Ok(format!("{} {}", user.name, *id))
}

fn main() {}
