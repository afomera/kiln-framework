use blog::{actions::notification_settings, records::*};
use kiln::{notify::Channel, prelude::*, testing::TestApp};

#[kiln::test]
async fn people_turn_every_email_off_and_on(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.sign_in_as(&user);
    client.get_html(notification_settings::index::path()).await.assert_status(200);
    client.patch_form(notification_settings::update::path("*", "mail", "off"), &[]).await.assert_flash("Turned off.");
    let mail = |prefs: &[kiln::notify::NotificationPref]| {
        prefs.iter().find(|p| p.kind == "*" && p.channel == Channel::Mail).map(|p| p.enabled)
    };
    assert_eq!(mail(&app.db.notification_prefs(&user).await?), Some(false));
    client.patch_form(notification_settings::update::path("*", "mail", "on"), &[]).await.assert_flash("Turned on.");
    assert_eq!(mail(&app.db.notification_prefs(&user).await?), Some(true));
    Ok(())
}

#[kiln::test]
async fn signed_out_people_cannot_change_settings(app: TestApp) -> Result {
    app.client().patch_form(notification_settings::update::path("*", "mail", "off"), &[]).await.assert_status(401);
    Ok(())
}

/// A notice nobody may mute: `optional()` lists no channels.
#[derive(Notification, serde::Serialize, serde::Deserialize, Debug)]
#[notification(description = "Security alert", optional())]
struct SecurityAlert {}

impl Notify for SecurityAlert {
    type To = User;

    async fn deliver(&self, _ctx: &Ctx, _to: &User, via: &mut Via) -> Result {
        via.inbox("New sign-in");
        Ok(())
    }
}

#[test]
fn empty_optional_means_nobody_can_mute_it() {
    use kiln::notify::NotificationInfo;
    assert!(SecurityAlert::OPTIONAL.is_empty());
    assert_eq!(blog::notifications::CommentPosted::OPTIONAL, &[Channel::Mail, Channel::Inbox]);
}
