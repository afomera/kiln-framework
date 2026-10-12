//! In-process HTTP testing: requests go through the real router with no socket.

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use tower::ServiceExt;

use crate::App;

pub struct TestClient {
    router: axum::Router,
}

impl TestClient {
    pub fn new(app: &App) -> Self {
        Self { router: app.router() }
    }

    pub async fn get(&self, path: impl AsRef<str>) -> TestResponse {
        self.request(Method::GET, path.as_ref(), None).await
    }

    /// Like [`TestClient::get`], but as a browser: `Accept: text/html`.
    pub async fn get_html(&self, path: impl AsRef<str>) -> TestResponse {
        self.request(Method::GET, path.as_ref(), Some("text/html,application/xhtml+xml,*/*;q=0.8")).await
    }

    pub async fn request(&self, method: Method, path: &str, accept: Option<&str>) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(accept) = accept {
            builder = builder.header(header::ACCEPT, accept);
        }
        let request = builder.body(Body::empty()).expect("valid test request");
        let response = self.router.clone().oneshot(request).await.expect("router is infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("readable body");
        TestResponse { status, headers, text: String::from_utf8_lossy(&bytes).into_owned() }
    }
}

#[derive(Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
}

impl TestResponse {
    #[track_caller]
    pub fn assert_status(&self, code: u16) -> &Self {
        assert_eq!(self.status.as_u16(), code, "unexpected status; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_contains(&self, needle: &str) -> &Self {
        assert!(self.text.contains(needle), "expected body to contain {needle:?}; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_not_contains(&self, needle: &str) -> &Self {
        assert!(!self.text.contains(needle), "expected body not to contain {needle:?}; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_redirect(&self, to: &str) -> &Self {
        assert!(self.status.is_redirection(), "expected a redirect, got {}", self.status);
        let location = self.headers.get(header::LOCATION).and_then(|v| v.to_str().ok());
        assert_eq!(location, Some(to), "unexpected redirect target");
        self
    }
}
