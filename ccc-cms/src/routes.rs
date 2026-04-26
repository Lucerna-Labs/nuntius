//! Request handlers + the CMS router builder.
//!
//! Public surface: `cms_router(store)` — returns an `axum::Router`
//! mounted at the workspace root. The router is responsible for
//! `/admin/setup`, `/admin/login`, `/admin/logout`, `/admin/`, plus
//! the static assets at `/admin/static`.

use std::sync::Arc;

use axum::{
    extract::{Form, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use tower_cookies::{Cookie, Cookies, CookieManagerLayer};
use tower_http::services::ServeDir;

use crate::auth::AdminStore;
use crate::middleware::{require_login, session_value, SESSION_COOKIE};
use crate::templates::{DashboardPage, LoginPage, NavSection, SetupPage};

/// Build the full `/admin` tree.
///
/// Combines:
/// - CMS-internal handlers (setup, login, logout, dashboard, static assets)
/// - Every other enabled plugin's `admin_routes()`, supplied as `plugin_admin`
///
/// The whole thing is wrapped in the auth middleware so anything beyond
/// setup/login/static requires a valid session.
///
/// Called by the host once at startup; if the CMS plugin is disabled the
/// host does not call this and `/admin` does not exist.
pub fn build_admin_tree(store: Arc<AdminStore>, plugin_admin: Router) -> Router {
    let static_dir = static_assets_dir();

    let inner = Router::new()
        .route("/setup", get(setup_get).post(setup_post))
        .route("/login", get(login_get).post(login_post))
        .route("/logout", get(logout))
        .route("/", get(dashboard))
        .with_state(store.clone())
        .merge(plugin_admin)
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(axum::middleware::from_fn_with_state(
            store.clone(),
            require_login,
        ))
        .layer(CookieManagerLayer::new());

    // axum 0.7's `nest("/admin", inner)` matches `/admin` but NOT `/admin/`
    // when the inner router has a `/` route. Add an explicit redirect so
    // browsers' implicit trailing slashes still land on the dashboard.
    Router::new()
        .nest("/admin", inner)
        .route("/admin/", get(|| async { Redirect::permanent("/admin") }))
}

fn static_assets_dir() -> std::path::PathBuf {
    // The `static` directory lives next to this crate's source. We embed
    // the path at compile time via CARGO_MANIFEST_DIR so the binary can
    // be relocated and still find its bundled assets — until we switch
    // to compile-time embedding (e.g. rust-embed) in a follow-up.
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("static");
    p
}

// ---------- Setup wizard ----------

#[derive(Deserialize)]
pub struct SetupForm {
    username: String,
    password: String,
    confirm: String,
}

async fn setup_get(State(store): State<Arc<AdminStore>>) -> Response {
    if store.is_initialized() {
        return Redirect::to("/admin/login").into_response();
    }
    SetupPage { error: None }.into_response()
}

async fn setup_post(
    State(store): State<Arc<AdminStore>>,
    cookies: Cookies,
    Form(form): Form<SetupForm>,
) -> Response {
    if store.is_initialized() {
        return Redirect::to("/admin/login").into_response();
    }

    if form.password != form.confirm {
        return SetupPage {
            error: Some("Passwords do not match."),
        }
        .into_response();
    }

    match store.create_admin(&form.username, &form.password) {
        Ok(()) => {
            // Auto-login after setup.
            issue_session_cookie(&store, &cookies, &form.username);
            Redirect::to("/admin/").into_response()
        }
        Err(e) => SetupPage {
            error: Some(&e.to_string()),
        }
        .into_response(),
    }
}

// ---------- Login ----------

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

async fn login_get(State(store): State<Arc<AdminStore>>) -> Response {
    if !store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }
    LoginPage {
        error: None,
        username: "",
    }
    .into_response()
}

async fn login_post(
    State(store): State<Arc<AdminStore>>,
    cookies: Cookies,
    Form(form): Form<LoginForm>,
) -> Response {
    if !store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }

    if store.verify_login(&form.username, &form.password) {
        issue_session_cookie(&store, &cookies, &form.username);
        Redirect::to("/admin/").into_response()
    } else {
        LoginPage {
            error: Some("Invalid username or password."),
            username: &form.username,
        }
        .into_response()
    }
}

// ---------- Logout ----------

async fn logout(State(store): State<Arc<AdminStore>>, cookies: Cookies) -> Response {
    let key = tower_cookies::Key::from(&store.cookie_key);
    let signed = cookies.signed(&key);
    signed.remove(Cookie::new(SESSION_COOKIE, ""));
    Redirect::to("/admin/login").into_response()
}

// ---------- Dashboard ----------

async fn dashboard(State(store): State<Arc<AdminStore>>) -> Response {
    let username = match store.admin() {
        Some(a) => a.username,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Plugin contributions to nav are wired in a follow-up; for now, a
    // stable placeholder so the dashboard renders even with no plugins.
    let nav: Vec<NavSection> = Vec::new();

    DashboardPage {
        admin_username: username,
        plugin_count: 0,
        plugin_names: Vec::new(),
        nav,
    }
    .into_response()
}

// ---------- Helpers ----------

fn issue_session_cookie(store: &AdminStore, cookies: &Cookies, username: &str) {
    let key = tower_cookies::Key::from(&store.cookie_key);
    let signed = cookies.signed(&key);
    let mut cookie = Cookie::new(SESSION_COOKIE, session_value(username));
    cookie.set_path("/admin");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    // `Secure` flag is set if the deployment terminates TLS; for
    // localhost development we leave it off so the cookie sticks.
    signed.add(cookie);
}
