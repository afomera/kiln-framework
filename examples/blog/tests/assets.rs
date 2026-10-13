//! Static files: fingerprinted assets, public files, and kiln's Turbo.

use blog::actions::posts;
use kiln::{prelude::*, testing::TestApp};

/// The fingerprinted URL the layout links for `name`.
fn linked<'a>(html: &'a str, prefix: &str) -> &'a str {
    let start = html.find(prefix).expect("asset is linked");
    let end = html[start..].find('"').expect("closing quote") + start;
    &html[start..end]
}

#[kiln::test]
async fn assets_are_fingerprinted_and_cached_forever(app: TestApp) -> Result {
    let page = app.get_html(posts::index::path()).await;
    let css = linked(&page.text, "/assets/app-");
    assert!(css.ends_with(".css"), "{css}");

    let res = app.get(css).await;
    res.assert_status(200).assert_contains("font: 16px/1.6");
    assert_eq!(res.headers["content-type"], "text/css; charset=utf-8");
    assert_eq!(res.headers["cache-control"], "public, max-age=31536000, immutable");
    assert!(res.headers.get("set-cookie").is_none(), "static files never set cookies");
    Ok(())
}

#[kiln::test]
async fn stale_fingerprints_still_serve_but_are_not_cached(app: TestApp) -> Result {
    let res = app.get("/assets/app-00000000.css").await;
    res.assert_status(200);
    assert_eq!(res.headers["cache-control"], "no-cache");
    Ok(())
}

#[kiln::test]
async fn paths_outside_assets_are_refused(app: TestApp) -> Result {
    app.get("/assets/../Cargo-00000000.toml").await.assert_status(404);
    app.get("/assets/%2e%2e/Cargo-00000000.toml").await.assert_status(404);
    Ok(())
}

#[kiln::test]
async fn public_files_are_served_at_the_root(app: TestApp) -> Result {
    let res = app.get("/robots.txt").await;
    res.assert_status(200).assert_contains("User-agent");
    assert_eq!(res.headers["content-type"], "text/plain; charset=utf-8");
    app.get("/favicon.svg").await.assert_status(200);
    app.get("/nope.txt").await.assert_status(404);
    Ok(())
}

#[kiln::test]
async fn turbo_ships_with_kiln_through_the_import_map(app: TestApp) -> Result {
    let page = app.get_html(posts::index::path()).await;
    page.assert_contains(r#"<script type="importmap">"#)
        .assert_contains(r#""@hotwired/turbo":"/kiln/turbo-8.0.12-"#)
        .assert_contains(r#""application":"/assets/application-"#)
        .assert_contains(r#"import "@hotwired/turbo"; import "kiln/confirm"; import "application";"#)
        .assert_contains(r#"<meta name="csrf-token""#);
    let turbo = linked(&page.text, "/kiln/turbo-");
    let res = app.get(turbo).await;
    res.assert_status(200).assert_contains("Turbo 8.0.12");
    assert_eq!(res.headers["content-type"], "text/javascript; charset=utf-8");
    Ok(())
}
