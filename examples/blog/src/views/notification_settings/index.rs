use kiln::{notify::NotificationPref, prelude::*};

#[derive(View)]
#[view(template = "notification_settings/index")]
pub struct Index {
    pub prefs: Vec<NotificationPref>,
}
