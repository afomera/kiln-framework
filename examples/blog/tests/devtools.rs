//! The development command center at /kiln.

use blog::records::*;
use kiln::{prelude::*, testing::TestApp};

#[kiln::test]
async fn kiln_redirects_to_the_dashboard(app: TestApp) -> Result {
    app.get("/kiln").await.assert_redirect("/kiln/dashboard");
    Ok(())
}

#[kiln::test]
async fn every_tool_renders(app: TestApp) -> Result {
    app.create::<Post>().await?;
    app.get("/kiln/dashboard").await.assert_status(200).assert_contains("href=\"/kiln/jobs\"");
    app.get("/kiln/routes").await.assert_status(200).assert_contains("posts::show");
    app.get("/kiln/records").await.assert_status(200).assert_contains("1 row(s)");
    for tool in ["jobs", "mail", "broadcasts", "sign_ins"] {
        app.get(format!("/kiln/{tool}")).await.assert_status(200);
    }
    Ok(())
}

/// The control token embedded in a /kiln page.
fn kiln_token(html: &str) -> String {
    let start = html.find("name=_kiln value=\"").expect("a control form") + "name=_kiln value=\"".len();
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

#[derive(kiln::Job, serde::Serialize, serde::Deserialize)]
struct Noop;

impl Perform for Noop {
    async fn perform(self, _ctx: &Ctx) -> Result {
        Ok(())
    }
}

#[kiln::test]
async fn controls_act_and_refuse_forgeries(app: TestApp) -> Result {
    let job = Noop.enqueue_in(&app.db, kiln::time::SignedDuration::from_hours(1)).await?;
    let page = app.get("/kiln/jobs").await;
    page.assert_status(200).assert_contains("Run now");
    let token = kiln_token(&page.text);

    let client = app.client();
    client.post_form_without_csrf(format!("/kiln/jobs/{job}/cancel"), &[("_kiln", "forged")]).await.assert_status(403);
    client
        .post_form_without_csrf(format!("/kiln/jobs/{job}/cancel"), &[("_kiln", &token), ("back", "/kiln/jobs")])
        .await
        .assert_status(303);
    app.assert_enqueued::<Noop>(0).await?;

    client.post_form_without_csrf("/kiln/queues/default/pause", &[("_kiln", &token)]).await.assert_status(303);
    app.get("/kiln/jobs").await.assert_contains("paused");
    client
        .post_form_without_csrf("/kiln/schedules/ArchiveOldDrafts/run", &[("_kiln", &token)])
        .await
        .assert_status(303);
    app.assert_enqueued::<blog::jobs::ArchiveOldDrafts>(1).await?;
    Ok(())
}

#[kiln::test]
async fn pages_filter(app: TestApp) -> Result {
    app.get("/kiln/routes?method=DELETE").await.assert_contains("posts::destroy").assert_not_contains("posts::show");
    app.get("/kiln/routes?q=sign_ins").await.assert_contains("sign_ins::index").assert_not_contains("posts::index");
    app.get("/kiln/records?q=slug").await.assert_contains("Post").assert_not_contains("Bookmark");
    Noop.enqueue(&app.db).await?;
    app.get("/kiln/jobs?q=Noop").await.assert_contains("Noop");
    app.get("/kiln/jobs?q=Nothing").await.assert_not_contains("<b>Noop</b>");
    Ok(())
}
