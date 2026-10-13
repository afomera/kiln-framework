use kiln::prelude::*;

use crate::records::User;

#[action(DELETE "/session")]
pub async fn destroy(ctx: Ctx, session: Session) -> Result<Redirect> {
    session.sign_out::<User>(&ctx.db).await?;
    Ok(Redirect::to("/").notice("Signed out."))
}
