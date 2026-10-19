use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::session::Flash;

/// A redirect, optionally with a flash message for the next page.
///
/// ```ignore
/// Ok(Redirect::to(posts::show::path(&post.slug)).notice("Post created."))
/// ```
///
/// [`Redirect::to`] and [`Redirect::back`] answer `303 See Other`, which is what a form
/// submission (and Turbo) wants: the browser follows it with a GET. [`Redirect::permanent`]
/// (308) and [`Redirect::temporary`] (307) are for URLs that moved. Flash messages work
/// with all of them.
#[derive(Debug, Clone)]
pub struct Redirect {
    to: String,
    status: StatusCode,
    flash: Flash,
}

impl Redirect {
    /// `303 See Other`, the right status after a form submission.
    pub fn to(path: impl Into<String>) -> Self {
        Self { to: path.into(), status: StatusCode::SEE_OTHER, flash: Flash::default() }
    }

    /// `308 Permanent Redirect`: the page lives at `path` now (an old slug, a renamed
    /// namespace), and browsers and search engines remember that. Unlike 301 it keeps the
    /// method and body, so a moved form endpoint still receives the POST.
    pub fn permanent(path: impl Into<String>) -> Self {
        Self { status: StatusCode::PERMANENT_REDIRECT, ..Self::to(path) }
    }

    /// `307 Temporary Redirect`: look at `path` this time, keeping the method and body.
    pub fn temporary(path: impl Into<String>) -> Self {
        Self { status: StatusCode::TEMPORARY_REDIRECT, ..Self::to(path) }
    }

    /// Back to the page the request came from (`303`), else to `fallback`: for small POSTs
    /// (follow, star, mark as read) that several pages share.
    ///
    /// Only the `Referer`'s path and query are used, and only when it names this site (its
    /// host is the request's `Host` or `X-Forwarded-Host`), so this is never an open
    /// redirect.
    ///
    /// ```ignore
    /// #[action(POST "/repos/{id}/star")]
    /// pub async fn create(ctx: Ctx, user: Current<User>, id: Param<Id<Repo>>, headers: HeaderMap) -> Result<Redirect> {
    ///     ..
    ///     Ok(Redirect::back(&headers, repos::show::path(*id)).notice("Starred."))
    /// }
    /// ```
    pub fn back(request: &impl RequestHeaders, fallback: impl Into<String>) -> Self {
        match same_site_referer(request.request_headers()) {
            Some(path) => Self::to(path),
            None => Self::to(fallback),
        }
    }

    /// Where it sends the browser.
    pub fn location(&self) -> &str {
        &self.to
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// A success message. Templates see it as `flash.notice`.
    pub fn notice(self, message: impl Into<String>) -> Self {
        self.flash("notice", message)
    }

    /// A warning or failure message. Templates see it as `flash.alert`.
    pub fn alert(self, message: impl Into<String>) -> Self {
        self.flash("alert", message)
    }

    /// After an op that may refuse with `Error::Conflict("Sorry, this event is full.")`:
    /// `notice` when it worked, the refusal as an alert when it didn't. Other errors pass
    /// through.
    ///
    /// ```ignore
    /// let back = Redirect::to(events::show::path(event.id));
    /// back.notice_or_alert(ops::create_rsvp(&ctx, new).await, "You're going!")
    /// ```
    pub fn notice_or_alert<T>(self, outcome: crate::Result<T>, notice: impl Into<String>) -> crate::Result<Self> {
        match outcome {
            Ok(_) => Ok(self.notice(notice)),
            Err(crate::Error::Conflict(reason)) => Ok(self.alert(reason)),
            Err(e) => Err(e),
        }
    }

    pub fn flash(mut self, kind: &str, message: impl Into<String>) -> Self {
        self.flash.0.insert(kind.to_owned(), message.into());
        self
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        let mut response = (self.status, [(header::LOCATION, self.to)]).into_response();
        if !self.flash.0.is_empty() {
            response.extensions_mut().insert(self.flash);
        }
        response
    }
}

/// What [`Redirect::back`] reads the `Referer` and `Host` from: a [`HeaderMap`] (an action's
/// `headers: HeaderMap` argument), a request, or request parts.
pub trait RequestHeaders {
    fn request_headers(&self) -> &HeaderMap;
}

impl RequestHeaders for HeaderMap {
    fn request_headers(&self) -> &HeaderMap {
        self
    }
}

impl<B> RequestHeaders for axum::http::Request<B> {
    fn request_headers(&self) -> &HeaderMap {
        self.headers()
    }
}

impl RequestHeaders for axum::http::request::Parts {
    fn request_headers(&self) -> &HeaderMap {
        &self.headers
    }
}

/// The `Referer`'s path and query, when it's this site's (see [`Redirect::back`]).
fn same_site_referer(headers: &HeaderMap) -> Option<String> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    let referer = text("referer")?;
    let (scheme, rest) = referer.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };
    // Browsers never send credentials in a Referer.
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    // Requests without a `Host` (only non-browser clients) are checked on the path alone.
    let forwarded = text("x-forwarded-host").and_then(|h| h.split(',').next()).map(str::trim);
    let hosts: Vec<&str> = [forwarded, text("host")].into_iter().flatten().collect();
    if !hosts.is_empty() && !hosts.iter().any(|host| host.eq_ignore_ascii_case(authority)) {
        return None;
    }
    let path = path.split('#').next().unwrap_or_default();
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_owned() };
    let safe = path.starts_with('/') && !path.starts_with("//") && !path.chars().any(|c| c.is_control() || c == '\\');
    safe.then_some(path)
}

/// A page, or a redirect instead: for actions that can't show their page, such as a reset
/// link that has expired.
///
/// ```ignore
/// pub async fn edit(..) -> Result<ViewOrRedirect<Edit>> {
///     let Some(user) = .. else { return Ok(ViewOrRedirect::Redirect(Redirect::to(..).alert(".."))) };
///     Ok(ViewOrRedirect::View(Edit { .. }))
/// }
/// ```
pub enum ViewOrRedirect<V> {
    View(V),
    Redirect(Redirect),
}

impl<V: IntoResponse> IntoResponse for ViewOrRedirect<V> {
    fn into_response(self) -> Response {
        match self {
            ViewOrRedirect::View(view) => view.into_response(),
            ViewOrRedirect::Redirect(redirect) => redirect.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_become_alerts() {
        let ok = Redirect::to("/").notice_or_alert(Ok(()), "Done.").unwrap();
        assert_eq!(ok.flash.0.get("notice").map(String::as_str), Some("Done."));
        let full = Redirect::to("/").notice_or_alert::<()>(Err(crate::Error::Conflict("Full.".into())), "Done.");
        assert_eq!(full.unwrap().flash.0.get("alert").map(String::as_str), Some("Full."));
        assert!(Redirect::to("/").notice_or_alert::<()>(Err(crate::Error::NotFound), "Done.").is_err());
    }

    #[test]
    fn statuses() {
        assert_eq!(Redirect::to("/a").into_response().status(), StatusCode::SEE_OTHER);
        assert_eq!(Redirect::permanent("/a").into_response().status(), StatusCode::PERMANENT_REDIRECT);
        let moved = Redirect::temporary("/a").notice("Moved.").into_response();
        assert_eq!(moved.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(moved.headers()[header::LOCATION], "/a");
        assert!(moved.extensions().get::<Flash>().is_some(), "flash rides along on every status");
    }

    fn back(pairs: &[(&'static str, &str)]) -> String {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, value.parse().unwrap());
        }
        Redirect::back(&headers, "/fallback").to
    }

    #[test]
    fn back_only_returns_to_this_site() {
        let host = ("host", "forked.sh");
        assert_eq!(back(&[host, ("referer", "https://forked.sh/ada/kiln?tab=stars#top")]), "/ada/kiln?tab=stars");
        assert_eq!(back(&[host, ("referer", "https://FORKED.sh")]), "/");
        assert_eq!(back(&[host, ("referer", "https://forked.sh?q=1")]), "/?q=1");
        // Behind a proxy, the public host comes in `X-Forwarded-Host`.
        let proxied =
            [("host", "10.0.0.5:3000"), ("x-forwarded-host", "forked.sh"), ("referer", "https://forked.sh/x")];
        assert_eq!(back(&proxied), "/x");
        for referer in [
            "https://evil.example/ada/kiln",
            "https://forked.sh.evil.example/x",
            "https://forked.sh@evil.example/x",
            "javascript://forked.sh/%0aalert(1)",
            "/relative",
            "https://forked.sh//evil.example/x",
            "https://forked.sh/\\evil.example",
        ] {
            assert_eq!(back(&[host, ("referer", referer)]), "/fallback", "{referer}");
        }
        assert_eq!(back(&[host]), "/fallback");
        // No `Host` (a test client, curl): the path alone decides.
        assert_eq!(back(&[("referer", "http://localhost:3000/posts")]), "/posts");
    }
}
