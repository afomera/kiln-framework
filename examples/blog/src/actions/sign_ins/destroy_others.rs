use kiln::prelude::*;

use crate::{actions::sign_ins, records::User};

#[action(DELETE "/settings/sign_ins")]
pub async fn destroy_others(ctx: Ctx, session: Session, user: Current<User>) -> Result<Redirect> {
    let count = session.sign_out_others(&ctx.db, &*user).await?;
    let devices = if count == 1 { "device" } else { "devices" };
    Ok(Redirect::to(sign_ins::index::path()).notice(format!("Signed out {count} other {devices}.")))
}
