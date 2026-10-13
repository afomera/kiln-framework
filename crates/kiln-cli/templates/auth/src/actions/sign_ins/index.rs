use kiln::prelude::*;

use crate::{records::User, views::sign_ins::Index};

/// Every browser and device the user is signed in on.
#[action(GET "/settings/sign_ins")]
pub async fn index(ctx: Ctx, session: Session, user: Current<User>) -> Result<Index> {
    Ok(Index { sign_ins: session.sign_ins(&ctx.db, &*user).await? })
}
