use kiln::prelude::*;

use crate::views::home::Index;

#[action(GET "/")]
pub async fn index() -> Index {
    Index {}
}
