//! Auth middleware.
//!
//! Gates `/admin/*` (except setup, login, static) on a signed session
//! cookie. Without a valid cookie, GET requests redirect to the login
//! page; non-GET requests get 401.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use std::sync::Arc;
use tower_cookies::Cookies;

use crate::auth::AdminStore;

/// Session cookie name.
pub const SESSION_COOKIE: &str = "ccc_session";

/// Routes that should bypass auth (setup, login, static assets).
///
/// Accepts both the full `/admin/...` form and the prefix-stripped form
/// that axum's `nest("/admin", ...)` shows to the inner middleware.
fn is_unauthenticated_path(path: &str) -> bool {
    matches!(
        path,
        "/admin/setup" | "/admin/login" | "/setup" | "/login"
    ) || path.starts_with("/admin/static/")
        || path.starts_with("/static/")
}

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
    State(store): State<Arc<AdminStore>>,
    cookies: Cookies,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    if is_unauthenticated_path(&path) {
        return next.run(req).await;
    }

    // First-run gate: nudge everyone to /admin/setup until the admin exists.
    if !store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }

    // Verify the session cookie against the configured admin.
    let key = tower_cookies::Key::from(&store.cookie_key);
    let signed = cookies.signed(&key);
    let valid = signed.get(SESSION_COOKIE).and_then(|c| {
        let raw = c.value().to_string();
        let user = parse_session(&raw)?.to_string();
        let admin = store.admin()?;
        if admin.username == user {
            Some(())
        } else {
            None
        }
    });

    if valid.is_some() {
        return next.run(req).await;
    }

    if method == axum::http::Method::GET {
        Redirect::to("/admin/login").into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
