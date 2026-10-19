//! What `#[action]` accepts beyond the obvious: a capture named like the action itself.
use std::path::Path;

use kiln::testing::TestApp;

mod tab {
    use kiln::prelude::*;

    /// The capture `{tab}` shares its name with the function; the generated handler keeps
    /// them apart.
    #[action(GET "/org/{slug}/{tab}")]
    pub async fn tab(slug: Param<String>, tab: Param<String>) -> String {
        format!("{} on {}", *tab, *slug)
    }
}

async fn app() -> TestApp {
    let migrations = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/db/migrations"));
    TestApp::configured(migrations, blog::app::configure).await.unwrap()
}

#[tokio::test]
async fn a_capture_can_share_the_actions_name() {
    assert_eq!(tab::path("kiln", "people"), "/org/kiln/people");
    let client = app().await.client();
    client.get("/org/kiln/people").await.assert_status(200).assert_contains("people on kiln");
}
