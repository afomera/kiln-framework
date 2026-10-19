//! What the layout reads as `layout_data`: computed once per page, after the action
//! (registered with `app.layout_data(..)` in `src/app.rs`).

use kiln::{Ctx, Result};
use serde::Serialize;

use crate::records::User;

#[derive(Serialize)]
pub struct LayoutData {
    /// For the nav's "Notifications (2)".
    pub unread: i64,
}

pub async fn data(ctx: Ctx, user: Option<User>) -> Result<LayoutData> {
    let unread = match &user {
        Some(user) => ctx.db.unread_notifications(user).await?,
        None => 0,
    };
    Ok(LayoutData { unread })
}
