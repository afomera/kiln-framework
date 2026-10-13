//! Turbo: Streams for partial page updates, Frames for scoped navigation.
//!
//! ```ignore
//! #[action(POST "/posts/{slug}/comments")]
//! pub async fn create(ctx: Ctx, turbo: Turbo, ..) -> Result<impl IntoResponse> {
//!     ..
//!     Ok(turbo.stream_or_redirect(
//!         TurboStream::new().append("comments", CommentItem { comment }),
//!         Redirect::to(posts::show::path(&post.slug)),
//!     ))
//! }
//! ```
//!
//! Stream content is a view (usually one with `layout = none`), rendered after the action
//! like any page, so templates can still read `current_user`.

use std::{convert::Infallible, fmt::Write as _};

use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{
    Redirect, Result, View,
    view::{PendingRender, escape_attribute},
};

pub const STREAM_MIME: &str = "text/vnd.turbo-stream.html";

/// What a request tells us about Turbo: whether it accepts stream responses, and which
/// frame (if any) it's loading.
#[derive(Debug, Clone, Default)]
pub struct Turbo {
    accepts_streams: bool,
    frame: Option<String>,
}

impl Turbo {
    /// Turbo form submissions accept `text/vnd.turbo-stream.html`.
    pub fn accepts_streams(&self) -> bool {
        self.accepts_streams
    }

    /// The `id` of the `<turbo-frame>` this request is loading, if any.
    pub fn frame(&self) -> Option<&str> {
        self.frame.as_deref()
    }

    /// The stream for Turbo requests, the redirect for everything else (plain HTML forms,
    /// tests without Turbo).
    pub fn stream_or_redirect(&self, stream: TurboStream, redirect: Redirect) -> StreamOrRedirect {
        if self.accepts_streams { StreamOrRedirect::Stream(stream) } else { StreamOrRedirect::Redirect(redirect) }
    }

    pub(crate) fn from_parts(parts: &Parts) -> Self {
        Self::from_headers(&parts.headers)
    }

    pub(crate) fn from_parts_of<B>(request: &axum::http::Request<B>) -> Self {
        Self::from_headers(request.headers())
    }

    fn from_headers(headers: &axum::http::HeaderMap) -> Self {
        let accepts_streams = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|accept| accept.contains(STREAM_MIME));
        let frame = headers.get("turbo-frame").and_then(|v| v.to_str().ok()).map(str::to_owned);
        Self { accepts_streams, frame }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Turbo {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts))
    }
}

/// One `<turbo-stream>` action. Content is rendered later, after the action returns.
#[derive(Debug, Clone)]
pub(crate) struct StreamAction {
    action: &'static str,
    target: String,
    content: Option<PendingRender>,
}

/// A list of Turbo Stream actions: a response to a Turbo request, or a broadcast.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct TurboStream {
    actions: Vec<StreamAction>,
}

impl TurboStream {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(mut self, action: &'static str, target: impl Into<String>, content: Option<PendingRender>) -> Self {
        self.actions.push(StreamAction { action, target: target.into(), content });
        self
    }

    /// Adds the view's HTML to the end of the element with id `target`. An existing child
    /// with the same id is replaced instead of duplicated.
    pub fn append(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("append", target, Some(PendingRender::of(&view)))
    }

    pub fn prepend(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("prepend", target, Some(PendingRender::of(&view)))
    }

    /// Replaces the element with id `target` (the element itself).
    pub fn replace(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("replace", target, Some(PendingRender::of(&view)))
    }

    /// Replaces the contents of the element with id `target`.
    pub fn update(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("update", target, Some(PendingRender::of(&view)))
    }

    pub fn before(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("before", target, Some(PendingRender::of(&view)))
    }

    pub fn after(self, target: impl Into<String>, view: impl View) -> Self {
        self.push("after", target, Some(PendingRender::of(&view)))
    }

    pub fn remove(self, target: impl Into<String>) -> Self {
        self.push("remove", target, None)
    }

    /// Asks the page to reload itself (Turbo's `refresh` action).
    pub fn refresh(mut self) -> Self {
        self.actions.push(StreamAction { action: "refresh", target: String::new(), content: None });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub(crate) fn contents(&self) -> impl Iterator<Item = &PendingRender> {
        self.actions.iter().filter_map(|a| a.content.as_ref())
    }

    /// The `<turbo-stream>` elements. Content views render without their layout.
    pub fn render(&self) -> Result<String> {
        let mut html = String::new();
        for action in &self.actions {
            match (&action.content, action.action) {
                (_, "refresh") => html.push_str("<turbo-stream action=\"refresh\"></turbo-stream>"),
                (None, _) => {
                    let _ = write!(
                        html,
                        "<turbo-stream action=\"{}\" target=\"{}\"></turbo-stream>",
                        action.action,
                        escape_attribute(&action.target)
                    );
                }
                (Some(content), _) => {
                    let _ = write!(
                        html,
                        "<turbo-stream action=\"{}\" target=\"{}\"><template>{}</template></turbo-stream>",
                        action.action,
                        escape_attribute(&action.target),
                        content.render_without_layout()?
                    );
                }
            }
        }
        Ok(html)
    }
}

/// A stream response, rendered by kiln after the action (see `PendingRender`).
#[derive(Debug, Clone)]
pub(crate) struct PendingStream(pub TurboStream);

impl IntoResponse for TurboStream {
    fn into_response(self) -> Response {
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(STREAM_MIME));
        response.extensions_mut().insert(PendingStream(self));
        response
    }
}

/// See [`Turbo::stream_or_redirect`].
pub enum StreamOrRedirect {
    Stream(TurboStream),
    Redirect(Redirect),
}

impl IntoResponse for StreamOrRedirect {
    fn into_response(self) -> Response {
        match self {
            StreamOrRedirect::Stream(stream) => stream.into_response(),
            StreamOrRedirect::Redirect(redirect) => redirect.into_response(),
        }
    }
}
