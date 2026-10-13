use kiln::prelude::*;

use crate::{actions::notifications, records::User};

/// Marks one notification read and follows its link.
#[action(PATCH "/notifications/{id}/read")]
pub async fn read(ctx: Ctx, user: Current<User>, id: Param<i64>) -> Result<Redirect> {
    let link = ctx.db.read_notification(&*user, *id).await?.and_then(|n| n.url);
    Ok(Redirect::to(link.unwrap_or_else(notifications::index::path)))
}
