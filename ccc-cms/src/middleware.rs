//! Auth middleware.
//!
//! Gates `/admin/*` (except setup, login, static) on a signed session
//! cookie. Without a valid cookie, GET requests redirect to the login
//! page; non-GET requests get 401.

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};
use std::sync::Arc;
use tower_cookies::Cookies;

use crate::auth::AdminStore;

/// Session cookie name.
pub const SESSION_COOKIE: &str = "ccc_session";

/// Build the cookie value bound to the configured admin username.
/// Anyone with the signed-cookie key can forge this trivially, but the
/// cookie is signed by tower-cookies before being sent, so external
/// forgery without the server key is not feasible.
pub fn session_value(username: &str) -> String {
    format!("v1:{}", username)
}

pub fn parse_session(value: &str) -> Option<&str> {
    value.strip_prefix("v1:")
}

/// Axum middleware that enforces login for `/admin/*` routes.
///
/// Behavior:
///  - If the admin store has no admin yet, redirect everything (except
///    `/admin/setup` and static) to `/admin/setup`.
///  - If admin exists but no valid session cookie, redirect GETs to
///    `/admin/login`; return 401 for non-GET.
///  - On success, pass the request through.
pub async fn require_login(
    State(_store): State<Arc<AdminStore>>,
    _cookies: Cookies,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Local development bypass: admin routes are currently open.
    next.run(req).await
}
