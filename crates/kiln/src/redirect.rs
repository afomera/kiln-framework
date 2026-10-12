use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::session::Flash;

/// A redirect, optionally with a flash message for the next page.
///
/// ```ignore
/// Ok(Redirect::to(posts::show::path(&post.slug)).notice("Post created."))
/// ```
#[derive(Debug, Clone)]
pub struct Redirect {
    to: String,
    flash: Flash,
}

impl Redirect {
    /// `303 See Other`, the right status after a form submission.
    pub fn to(path: impl Into<String>) -> Self {
        Self { to: path.into(), flash: Flash::default() }
    }

    /// A success message. Templates see it as `flash.notice`.
    pub fn notice(self, message: impl Into<String>) -> Self {
        self.flash("notice", message)
    }

    /// A warning or failure message. Templates see it as `flash.alert`.
    pub fn alert(self, message: impl Into<String>) -> Self {
        self.flash("alert", message)
    }

    pub fn flash(mut self, kind: &str, message: impl Into<String>) -> Self {
        self.flash.0.insert(kind.to_owned(), message.into());
        self
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, self.to)]).into_response();
        if !self.flash.0.is_empty() {
            response.extensions_mut().insert(self.flash);
        }
        response
    }
}
