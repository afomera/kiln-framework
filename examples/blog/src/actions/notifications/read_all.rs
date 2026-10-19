use kiln::prelude::*;

use crate::{actions::notifications, records::User};

/// Marks everything read, then returns to the page the button was on (the inbox if that
/// can't be told).
#[action(PATCH "/notifications/read")]
pub async fn read_all(ctx: Ctx, user: Current<User>, headers: HeaderMap) -> Result<Redirect> {
    let count = ctx.db.mark_all_notifications_read(&*user).await?;
    Ok(Redirect::back(&headers, notifications::index::path()).notice(format!("Marked {count} read.")))
}
