use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::error_page::{ErrorReport, SourceExcerpt};

pub type Result<T = (), E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("no `{table}` row matched the query")]
    MissingRecord { table: &'static str },
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),
    #[error("data integrity error: {0}")]
    Integrity(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::NotFound | Error::MissingRecord { .. } => StatusCode::NOT_FOUND,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable code for this kind of error. `kiln explain <code>` will document each one.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound => "K1002",
            Error::MissingRecord { .. } => "K1003",
            Error::BadRequest(_) => "K1005",
            Error::Template(_) => "K1101",
            Error::Database(_) => "K1201",
            Error::Integrity(_) => "K1202",
            Error::Io(_) => "K1301",
        }
    }

    pub(crate) fn report(&self) -> ErrorReport {
        let mut report = ErrorReport::new(self.status(), self.code(), self.title(), self.to_string());
        match self {
            Error::NotFound => {
                report.hint("An action returned `Error::NotFound`. Visitors see a plain Not Found page in production.");
            }
            Error::MissingRecord { table } => {
                report.message = format!("No `{table}` row matched the query.");
                report.hint("`one_or_404` turns a missing row into this 404. Check the path parameter and any filters in the action's query.");
                report.hint("Visitors see a plain Not Found page in production.");
            }
            Error::BadRequest(message) => {
                report.message = message.clone();
                if message.contains("path parameter") {
                    report.hint("The URL segment didn't parse into the type the action declares, e.g. `id: Param<i64>` given `abc`. Build links with the generated `path(..)` helper so they always match.");
                }
            }
            Error::Template(e) => template_report(&mut report, e),
            Error::Database(e) => {
                report.message = e.to_string();
                let text = e.to_string();
                if text.contains("UNIQUE constraint failed") {
                    report.hint("A unique index rejected the write. Check for an existing row first, or validate uniqueness before inserting.");
                } else if text.contains("FOREIGN KEY constraint failed") {
                    report.hint("A foreign key points at a row that doesn't exist. Insert the parent first, or check the id you're passing.");
                } else if text.contains("no such table") || text.contains("no such column") {
                    report.hint("The database schema is behind the record structs. Create or migrate the table (`db.create_table::<T>()` in M0).");
                } else {
                    report.hint("The message above comes from SQLite through sqlx.");
                }
            }
            Error::Integrity(message) => {
                report.message = message.clone();
                report.hint("A relation points at a row that no longer exists. Check deletes on the parent table.");
            }
            Error::Io(e) => {
                report.message = e.to_string();
            }
        }
        report
    }

    fn title(&self) -> &'static str {
        match self {
            Error::NotFound => "Not found",
            Error::MissingRecord { .. } => "Record not found",
            Error::BadRequest(_) => "Bad request",
            Error::Template(_) => "Template error",
            Error::Database(_) => "Database error",
            Error::Integrity(_) => "Data integrity error",
            Error::Io(_) => "I/O error",
        }
    }
}

fn template_report(report: &mut ErrorReport, e: &minijinja::Error) {
    report.message = match e.detail() {
        Some(detail) => format!("{}: {detail}", e.kind()),
        None => e.kind().to_string(),
    };
    // Errors inside an included or extended template carry the inner error as their source.
    let mut inner = e;
    while let Some(next) = std::error::Error::source(inner).and_then(|s| s.downcast_ref::<minijinja::Error>()) {
        inner = next;
    }
    if let (Some(name), Some(line), Some(source)) = (inner.name(), inner.line(), inner.template_source()) {
        report.source = Some(SourceExcerpt::new(format!("templates/{name}"), line, source, "jinja"));
        // Name the exact expression that failed, e.g. "`post.word_count` is undefined".
        let expr = inner.range().and_then(|r| source.get(r)).map(str::trim).filter(|x| !x.is_empty() && x.len() < 80);
        if let (Some(expr), minijinja::ErrorKind::UndefinedError) = (expr, e.kind()) {
            report.message = format!("`{expr}` is undefined");
        }
    }
    match e.kind() {
        minijinja::ErrorKind::UndefinedError => {
            report.hint("Templates are strict: a missing value is an error, never an empty string.");
            report.hint("Check that the view struct has the field, the relation was preloaded (`.preload(post::author)`), or the `#[part]` method exists.");
            report.hint(
                "A layout only sees `content`, the view's fields, and top-level `{% set %}` values from the page.",
            );
        }
        minijinja::ErrorKind::TemplateNotFound => {
            report.hint("Template paths are relative to `templates/` and end in `.html.jinja`.");
        }
        minijinja::ErrorKind::SyntaxError => {
            report.hint(
                "Fix the syntax at the highlighted line. In debug builds the template reloads on the next request.",
            );
        }
        _ => {}
    }
}

impl IntoResponse for Error {
    /// The body here is a plain fallback. Kiln's request middleware replaces it with the
    /// full error page (HTML or Markdown) and adds the request details.
    fn into_response(self) -> Response {
        let status = self.status();
        let report = self.report();
        let mut response = (status, format!("{} {}: {}", status.as_u16(), report.code, report.title)).into_response();
        response.extensions_mut().insert(report);
        response
    }
}
