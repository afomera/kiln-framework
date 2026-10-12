use kiln::prelude::*;

use crate::{actions::home, records::User};

#[action(DELETE "/session")]
pub async fn destroy(session: Session) -> Redirect {
    session.sign_out::<User>();
    Redirect::to(home::index::path()).notice("Signed out.")
}
