use kiln::{notify::InboxItem, prelude::*};

#[derive(View)]
#[view(template = "notifications/index")]
pub struct Index {
    pub notifications: Vec<InboxItem>,
    pub unread: i64,
}
