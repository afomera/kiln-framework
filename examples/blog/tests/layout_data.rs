//! `app.layout_data(..)`: what happens when the hook fails, or a layout reads it without one.

use std::path::Path;

use blog::{actions::posts, records::*};
use kiln::{prelude::*, testing::TestApp};

async fn app_with(configure: impl FnOnce(kiln::App) -> Result<kiln::App>) -> TestApp {
    let migrations = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/db/migrations"));
    TestApp::configured(migrations, configure).await.expect("the test app boots")
}

async fn failing(_ctx: Ctx, _user: Option<User>) -> Result<i64> {
    Err(Error::Conflict("the unread count is unavailable".into()))
}

#[tokio::test]
async fn a_failing_hook_fails_the_page_and_says_where_it_came_from() {
    let app = app_with(|app| Ok(blog::app::configure(app)?.layout_data(failing))).await;
    let page = app.client().get(posts::index::path()).await;
    page.assert_status(409)
        .assert_contains("the unread count is unavailable")
        .assert_contains("Raised by the app's `app.layout_data(..)` hook");
    // JSON responses render no templates, so the hook never runs for them.
    app.client().get(blog::actions::api::posts::index::path()).await.assert_status(200);
}

#[tokio::test]
async fn reading_layout_data_without_a_hook_is_a_clear_error() {
    let app =
        app_with(|app| Ok(app.signs_in::<User, _>("/session/new", blog::views::current_user::CurrentUser::of))).await;
    app.client()
        .get(posts::index::path())
        .await
        .assert_status(500)
        .assert_contains("registers no `app.layout_data(..)` hook");
}
