use kiln::prelude::*;

use crate::{actions::notifications, records::User};

/// Marks one notification read and follows its link.
#[action(PATCH "/notifications/{id}/read")]
pub async fn read(ctx: Ctx, user: Current<User>, id: Param<i64>) -> Result<Redirect> {
    ctx.db.mark_notification_read(&*user, *id).await?;
    let link = ctx.db.inbox(&*user, 200).await?.into_iter().find(|n| n.id == *id).and_then(|n| n.url);
    Ok(Redirect::to(link.unwrap_or_else(notifications::index::path)))
}
