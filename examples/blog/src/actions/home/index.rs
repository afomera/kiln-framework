use kiln::prelude::*;

use crate::actions::posts;

#[action(GET "/")]
pub async fn index() -> Redirect {
    Redirect::to(posts::index::path())
}
