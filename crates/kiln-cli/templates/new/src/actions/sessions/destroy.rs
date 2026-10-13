use kiln::prelude::*;

use crate::{actions::home, records::User};

#[action(DELETE "/session")]
pub async fn destroy(ctx: Ctx, session: Session) -> Result<Redirect> {
    session.sign_out::<User>(&ctx.db).await?;
    Ok(Redirect::to(home::index::path()).notice("Signed out."))
}
