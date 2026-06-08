//! Render an askama template into an axum `Response`. Returns 500 on
//! template failure (which is always a programmer error — the template
//! was already type-checked at compile time, so a render failure means
//! the data is malformed).

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

/// Renders `tmpl` to HTML, or returns a 500 on render failure.
pub fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
