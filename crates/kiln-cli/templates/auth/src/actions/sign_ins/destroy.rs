use kiln::prelude::*;

use crate::{actions::sign_ins, records::User};

/// Signs out one device. Ids of other people's sign-ins are ignored.
#[action(DELETE "/settings/sign_ins/{id}")]
pub async fn destroy(ctx: Ctx, session: Session, user: Current<User>, id: Param<i64>) -> Result<Redirect> {
    let message = if session.revoke(&ctx.db, &*user, *id).await? { "Signed out that device." } else { "Already signed out." };
    Ok(Redirect::to(sign_ins::index::path()).notice(message))
}
