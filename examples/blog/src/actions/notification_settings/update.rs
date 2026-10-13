use kiln::{notify::Channel, prelude::*};

use crate::{actions::notification_settings, records::User};

/// Turns one channel on or off: `kind` is a notification type, or `*` for all of them.
#[action(PATCH "/settings/notifications/{kind}/{channel}/{state}")]
pub async fn update(
    ctx: Ctx,
    user: Current<User>,
    kind: Param<String>,
    channel: Param<String>,
    state: Param<String>,
) -> Result<Redirect> {
    let channel: Channel = channel.parse()?;
    let on = state.as_str() == "on";
    ctx.db.set_notification_pref(&*user, &kind, channel, on).await?;
    Ok(Redirect::to(notification_settings::index::path()).notice(if on { "Turned on." } else { "Turned off." }))
}
