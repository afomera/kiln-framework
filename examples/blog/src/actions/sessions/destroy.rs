use kiln::prelude::*;

use crate::{actions::posts, records::User};

#[action(DELETE "/session")]
pub async fn destroy(session: Session) -> Redirect {
    session.sign_out::<User>();
    Redirect::to(posts::index::path()).notice("Signed out.")
}
