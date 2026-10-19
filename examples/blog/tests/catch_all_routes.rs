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

/// A profile page at the root, like forked.sh's `/{owner}`: it matches `/favicon.svg` too.
mod profile {
    use kiln::prelude::*;

    #[action(GET "/{owner}")]
    pub async fn show(owner: Param<String>) -> String {
        format!("profile of {}", *owner)
    }
}

/// A route written for exactly a `public/` file's path.
mod robots {
    use kiln::prelude::*;

    #[action(GET "/robots.txt")]
    pub async fn show() -> &'static str {
        "User-agent: *\nDisallow: /drafts"
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

#[tokio::test]
async fn public_files_win_over_routes_that_merely_match_them() {
    let client = app().await.client();
    let favicon = client.get("/favicon.svg").await;
    favicon.assert_status(200).assert_contains("<svg");
    assert_eq!(favicon.headers["content-type"], "image/svg+xml");
    client.request(kiln::__private::axum::http::Method::HEAD, "/favicon.svg", None).await.assert_status(200);
    // Paths that aren't files still reach the route.
    client.get("/ada").await.assert_contains("profile of ada");
    client.get("/missing.svg").await.assert_contains("profile of missing.svg");
    // A route for exactly the path is the app's choice, so it wins over the file.
    client.get("/robots.txt").await.assert_contains("Disallow: /drafts");
}
