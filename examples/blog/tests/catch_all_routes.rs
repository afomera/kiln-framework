//! A catch-all `{*path}` capture takes the rest of the URL, slashes included, for file
//! browsers and branch names like `feature/login`.
use std::path::Path;

use kiln::testing::TestApp;

mod tree {
    use kiln::prelude::*;

    #[action(GET "/{owner}/{repo}/tree/{*path}")]
    pub async fn tree(owner: Param<String>, repo: Param<String>, path: Param<String>) -> String {
        format!("{}/{} at {:?}", *owner, *repo, *path)
    }
}

async fn app() -> TestApp {
    let migrations = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/db/migrations"));
    TestApp::configured(migrations, blog::app::configure).await.unwrap()
}

#[tokio::test]
async fn catch_all_receives_the_rest_of_the_path() {
    let client = app().await.client();
    client.get("/a/b/tree/src/lib/mod.rs").await.assert_status(200).assert_contains(r#"a/b at "src/lib/mod.rs""#);
    client.get("/a/b/tree/README").await.assert_contains(r#"a/b at "README""#);
    // Segments are decoded one by one, so an encoded slash is a slash.
    client.get("/a/b/tree/src%2Flib/my%20file.rs").await.assert_contains(r#"a/b at "src/lib/my file.rs""#);
    // A catch-all matches at least one character.
    client.get("/a/b/tree/").await.assert_status(404);
    // Literal routes still win over a catch-all at the root.
    client.get("/posts").await.assert_status(200);
}

#[tokio::test]
async fn path_helper_round_trips() {
    let path = tree::path("ada", "kiln", "docs/getting started.md");
    assert_eq!(path, "/ada/kiln/tree/docs/getting%20started.md");
    let client = app().await.client();
    client.get(&path).await.assert_contains(r#"ada/kiln at "docs/getting started.md""#);
}
