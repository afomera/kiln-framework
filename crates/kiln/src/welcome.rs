//! The welcome page: what `/` shows outside production until the app declares a route for
//! it (or adds `public/index.html`), like Rails' default home page. A self-contained page
//! (`welcome.html`: inline CSS, JS and SVG, no network requests).

use axum::{
    body::Body,
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};

/// Placeholder links, until these pages exist.
const GUIDES_URL: &str = "https://afomera.dev/kiln/guides";
const DOCS_URL: &str = "https://afomera.dev/kiln/docs";
const GITHUB_URL: &str = "https://afomera.dev/kiln/github";

const PAGE: &str = include_str!("welcome.html");

/// Lucide `copy` / `check` (ISC license, lucide.dev), shown once JS can use the clipboard.
const COPY_BUTTON: &str = "<button type=\"button\" class=\"copy\" data-copy hidden aria-label=\"Copy command\">\
<svg class=\"clip\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><rect width=\"14\" height=\"14\" x=\"8\" y=\"8\" rx=\"2\" ry=\"2\"/><path d=\"M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2\"/></svg>\
<svg class=\"check\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"M20 6 9 17l-5-5\"/></svg></button>";

/// Lucide `arrow-up-right`.
const ARROW: &str = "<svg class=\"arrow\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"M7 7h10v10\"/><path d=\"M7 17 17 7\"/></svg>";

/// Whether an unmatched request gets the welcome page.
pub(crate) fn applies(method: &Method, path: &str, production: bool) -> bool {
    !production && path == "/" && matches!(*method, Method::GET | Method::HEAD)
}

/// The page for the app in `root` (its name comes from `Cargo.toml`).
pub(crate) fn page(root: &std::path::Path) -> Response {
    let crate_name = crate::cli::app_name(root);
    let dir = root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| crate_name.clone());
    let html = render(&title(&crate_name), &dir, env!("CARGO_PKG_VERSION"), crate::config::env());
    let mut response = (StatusCode::OK, Body::from(html)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn render(app: &str, dir: &str, version: &str, env: &str) -> String {
    PAGE.replace("@@copy@@", COPY_BUTTON)
        .replace("@@arrow@@", ARROW)
        .replace("@@guides_url@@", GUIDES_URL)
        .replace("@@docs_url@@", DOCS_URL)
        .replace("@@github_url@@", GITHUB_URL)
        .replace("@@version@@", version)
        .replace("@@env@@", &esc(env))
        .replace("@@dir@@", &esc(dir))
        .replace("@@app@@", &esc(app))
}

/// `my_blog` → `My blog`, as `kiln new` titles the app.
fn title(crate_name: &str) -> String {
    let words = crate_name.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_for_root_outside_production() {
        assert!(applies(&Method::GET, "/", false));
        assert!(applies(&Method::HEAD, "/", false));
        assert!(!applies(&Method::GET, "/", true));
        assert!(!applies(&Method::GET, "/posts", false));
        assert!(!applies(&Method::POST, "/", false));
    }

    #[test]
    fn renders_the_app() {
        let html = render("My <blog>", "my_blog", "1.2.3", "development");
        assert!(html.contains("<span class=\"name\">My &lt;blog&gt;</span>"));
        assert!(html.contains("v1.2.3"));
        assert!(html.contains("~/my_blog"));
        assert!(html.contains("href=\"/kiln\""));
        assert!(html.contains(DOCS_URL));
        assert!(!html.contains("@@"), "every placeholder is filled");
    }

    /// A route other than `/`, so the test app has one.
    #[crate::action(GET "/welcome-test/elsewhere")]
    async fn elsewhere() -> crate::Redirect {
        crate::Redirect::to("/")
    }

    /// An app with no `/` route (kiln's own test binary declares none) gets the page.
    #[tokio::test]
    async fn served_when_no_route_matches_root() {
        let db = crate::Db::memory().await.unwrap();
        let app = crate::App::new(db);
        let client = crate::testing::TestClient::new(&app);
        client
            .get_html("/")
            .await
            .assert_status(200)
            .assert_contains("is running.")
            .assert_contains("kiln g resource Post");
        client.get_html("/nope").await.assert_status(404);
    }
}
