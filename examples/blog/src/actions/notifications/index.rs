use kiln::prelude::*;

use crate::{records::User, views::notifications::Index};

/// The signed-in user's inbox.
#[action(GET "/notifications")]
pub async fn index(ctx: Ctx, user: Current<User>) -> Result<Index> {
    let notifications = ctx.db.inbox(&*user, 50).await?;
    let unread = ctx.db.unread_notifications(&*user).await?;
    Ok(Index { notifications, unread })
}
