use kiln::prelude::*;

use crate::{records::User, views::notification_settings::Index};

/// What the signed-in user gets, and where.
#[action(GET "/settings/notifications")]
pub async fn index(ctx: Ctx, user: Current<User>) -> Result<Index> {
    Ok(Index { prefs: ctx.db.notification_prefs(&*user).await? })
}
