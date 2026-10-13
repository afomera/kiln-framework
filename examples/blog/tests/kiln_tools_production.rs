//! `/kiln` in production: only for viewers `kiln_tools_for` allows. A separate test binary,
//! because the environment is fixed per process.

use std::path::Path;

use blog::records::*;
use kiln::{App, prelude::*, testing::TestApp};

fn production() {
    // Every test in this binary runs as production, before any app exists.
    unsafe { std::env::set_var("KILN_ENV", "production") };
}

async fn app(tools: bool) -> Result<TestApp> {
    production();
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("db/migrations");
    TestApp::configured(&migrations, move |app: App| {
        let app = blog::app::configure(app)?;
        // An app would check a column, e.g. `|user| user.admin`.
        Ok(if tools { app.kiln_tools_for::<User>(|user| user.email == "staff@example.com") } else { app })
    })
    .await
}

fn kiln_token(html: &str) -> String {
    let start = html.find("name=_kiln value=\"").expect("a control form") + "name=_kiln value=\"".len();
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

#[tokio::test]
async fn without_kiln_tools_for_the_pages_do_not_exist() -> Result {
    let app = app(false).await?;
    assert_eq!(kiln::config::env(), "production");
    let staff = app.create_with::<User>(|u| u.email = "staff@example.com".into()).await?;
    app.sign_in_as(&staff).get("/kiln/dashboard").await.assert_status(404);
    Ok(())
}

#[tokio::test]
async fn only_allowed_viewers_see_and_use_them() -> Result {
    let app = app(true).await?;
    let staff = app.create_with::<User>(|u| u.email = "staff@example.com".into()).await?;
    let reader = app.create::<User>().await?;

    app.get("/kiln/dashboard").await.assert_status(404);
    app.sign_in_as(&reader).get("/kiln/jobs").await.assert_status(404);

    let staff_client = app.sign_in_as(&staff);
    let page = staff_client.get("/kiln/jobs").await;
    page.assert_status(200).assert_contains(&format!("users #{}", staff.id)).assert_contains("production");

    app.db.pause_queue("mail").await?;
    let page = staff_client.get("/kiln/jobs").await;
    let token = kiln_token(&page.text);
    staff_client.post_form_without_csrf("/kiln/queues/mail/resume", &[("_kiln", &token)]).await.assert_status(303);
    assert!(app.db.queues().await?.iter().all(|(_, paused, ..)| !paused));

    // Someone else can't use the staff member's token.
    app.db.pause_queue("mail").await?;
    let reader_post =
        app.sign_in_as(&reader).post_form_without_csrf("/kiln/queues/mail/resume", &[("_kiln", &token)]).await;
    assert_ne!(reader_post.status, 303);
    assert!(app.db.queues().await?.iter().any(|(q, paused, ..)| q == "mail" && *paused));
    Ok(())
}
