use kiln::prelude::*;

use crate::{actions::notifications, records::User};

#[action(PATCH "/notifications/read")]
pub async fn read_all(ctx: Ctx, user: Current<User>) -> Result<Redirect> {
    let count = ctx.db.mark_all_notifications_read(&*user).await?;
    Ok(Redirect::to(notifications::index::path()).notice(format!("Marked {count} read.")))
}
