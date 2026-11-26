//! Auth middleware.
//!
//! Gates `/admin/*` (except setup, login, static) on a signed session
//! cookie. Without a valid cookie, GET requests redirect to the login
//! page; non-GET requests get 401.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use std::sync::Arc;
use tower_cookies::{Cookies, Key};

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

/// Paths under `/admin` that must stay reachable without a session: the
/// setup wizard, the login form, and the shell's static assets (the login
/// page itself links the stylesheet).
fn is_exempt(path: &str) -> bool {
    path == "/setup" || path == "/login" || path == "/static" || path.starts_with("/static/")
}

/// True iff the request carries a signed `ccc_session` cookie whose
/// username matches the configured admin account. Shared between the
/// middleware and `login_get` (which skips the form for signed-in users).
pub fn session_is_valid(store: &AdminStore, cookies: &Cookies) -> bool {
    let admin = match store.admin() {
        Some(a) => a,
        None => return false,
    };
    let key = Key::from(&store.cookie_key);
    match cookies.signed(&key).get(SESSION_COOKIE) {
        // Trimmed comparison for the same reason verify_login trims:
        // accounts created before usernames were trimmed on write.
        Some(cookie) => {
            parse_session(cookie.value()).map(str::trim) == Some(admin.username.trim())
        }
        None => false,
    }
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
    // Operator explicitly switched the gate off in ccc.toml
    // ([plugin.cms] disable_auth = true). Everything passes.
    if store.auth_disabled {
        return next.run(req).await;
    }

    // This layer sits on the router that gets nested under `/admin`, so
    // axum has already stripped that prefix from the URI it sees here.
    // Strip defensively anyway in case the tree is ever mounted un-nested.
    let path = req.uri().path();
    let path = path.strip_prefix("/admin").unwrap_or(path);

    if is_exempt(path) {
        return next.run(req).await;
    }

    // First run: no account on disk yet — funnel everything to setup.
    if !store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }

    if session_is_valid(&store, &cookies) {
        return next.run(req).await;
    }

    if req.method() == Method::GET {
        Redirect::to("/admin/login").into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Response-header middleware for the admin tree. Conservative set only —
/// deliberately no CSP, because plugin admin pages rely on inline
/// styles/scripts and a policy strict enough to matter would break them.
pub async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("SAMEORIGIN"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    res
}

/// Inject the persistent left-side AI Command Dock assets into every HTML
/// page under `/admin/*` (excluding auth pages, static assets, and embedded
/// WordCraft/PdfCraft WASM iframe apps) so the AI Team stays available
/// across all tabs and platforms automatically.
pub async fn inject_agent_dock(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let res = next.run(req).await;

    if method != Method::GET {
        return res;
    }
    let rel = path.strip_prefix("/admin").unwrap_or(&path);
    if is_exempt(rel)
        || rel.starts_with("/wordcraft/app")
        || rel.starts_with("/pdfcraft/app")
        || rel.starts_with("/api/")
    {
        return res;
    }

    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false);
    if !is_html {
        return res;
    }

    let (mut parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, 4 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };
    let html = String::from_utf8_lossy(&bytes);
    if html.contains("agent-dock.js") || !html.contains("</body>") {
        return Response::from_parts(parts, Body::from(bytes));
    }

    let snippet = concat!(
        "<link rel=\"stylesheet\" href=\"/admin/static/agent-dock.css\">",
        "<script src=\"/admin/static/agent-dock.js\" defer></script>",
        "</body>"
    );
    let injected = html.replacen("</body>", snippet, 1);
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(injected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_cookies::cookie::{Cookie, CookieJar};

    #[test]
    fn session_value_round_trip() {
        assert_eq!(parse_session(&session_value("alice")), Some("alice"));
        assert_eq!(parse_session("v2:alice"), None);
        assert_eq!(parse_session("garbage"), None);
    }

    #[test]
    fn exempt_paths() {
        assert!(is_exempt("/setup"));
        assert!(is_exempt("/login"));
        assert!(is_exempt("/static/admin.css"));
        assert!(!is_exempt("/"));
        assert!(!is_exempt("/radio"));
        assert!(!is_exempt("/logout"));
        assert!(!is_exempt("/api/plugins"));
    }

    #[test]
    fn signed_cookie_round_trip_and_tamper() {
        let key = Key::generate();
        let mut jar = CookieJar::new();
        jar.signed_mut(&key)
            .add(Cookie::new(SESSION_COOKIE, session_value("alice")));

        // The raw value on the wire is signed — not the bare payload.
        let raw = jar.get(SESSION_COOKIE).unwrap().value().to_string();
        assert_ne!(raw, session_value("alice"));

        // Verification strips the signature and yields the payload back.
        let got = jar.signed(&key).get(SESSION_COOKIE).unwrap();
        assert_eq!(parse_session(got.value()), Some("alice"));

        // A tampered value fails signature verification entirely.
        let mut evil = CookieJar::new();
        evil.add(Cookie::new(SESSION_COOKIE, format!("{}x", raw)));
        assert!(evil.signed(&key).get(SESSION_COOKIE).is_none());
    }
}
