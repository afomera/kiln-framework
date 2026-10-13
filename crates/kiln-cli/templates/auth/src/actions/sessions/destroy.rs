use kiln::prelude::*;

use crate::records::User;

/// Signs this browser out; other devices stay signed in (see /settings/sign_ins).
#[action(DELETE "/session")]
pub async fn destroy(ctx: Ctx, session: Session) -> Result<Redirect> {
    session.sign_out::<User>(&ctx.db).await?;
    Ok(Redirect::to("/").notice("Signed out."))
}
