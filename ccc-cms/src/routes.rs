//! Request handlers + the CMS router builder.
//!
//! Public surface: `build_admin_tree(ctx, plugin_admin)` — assembles the
//! `/admin/*` tree (CMS-internal handlers + every plugin's admin_routes,
//! all wrapped in the auth middleware) and returns it as an `axum::Router`
//! ready to merge into the host's app router.

use std::sync::Arc;

use axum::{
    extract::{Form, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Redirect, Response},
    routing::get,
    Router,
};
use ccc_core::{AdminNavEntry, AppContext, PluginManager};
use serde::Deserialize;
use tower_cookies::{Cookie, CookieManagerLayer, Cookies};
use tower_http::services::ServeDir;

use crate::auth::AdminStore;
use crate::middleware::{
    inject_agent_dock, require_login, security_headers, session_is_valid, session_value,
    SESSION_COOKIE,
};
use crate::templates::{DashboardPage, LoginPage, SetupPage};

/// State assembled by the host and passed into the admin tree.
///
/// Carries the auth store, the plugin manager (so the admin shell can
/// enumerate plugins and pull live status), and the app context the
/// manager needs to call into individual plugins. Cheap to clone —
/// internal data is `Arc`-shared.
#[derive(Clone)]
pub struct DashboardCtx {
    pub store: Arc<AdminStore>,
    /// Legacy nav-entry list. The Nuntius shell does not use it (it
    /// builds the rail from `/admin/api/plugins`), but we keep it for
    /// templates that still reference it.
    pub nav: Vec<AdminNavEntry>,
    pub plugin_names: Vec<String>,
    pub plugin_manager: Arc<PluginManager>,
    pub app_ctx: AppContext,
}

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
pub fn build_admin_tree(ctx: DashboardCtx, plugin_admin: Router) -> Router {
    let static_dir = static_assets_dir();
    let store = ctx.store.clone();

    let inner = Router::new()
        .route("/setup", get(setup_get).post(setup_post))
        .route("/login", get(login_get).post(login_post))
        // GET keeps the shell's plain "Sign out" anchors working; POST is
        // the CSRF-safer form for programmatic clients.
        .route("/logout", get(logout).post(logout))
        .route("/", get(dashboard))
        .route("/api/plugins", get(api_plugins))
        .route("/api/plugins/:id/status", get(api_plugin_status))
        .with_state(ctx)
        .merge(plugin_admin)
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(axum::middleware::from_fn(inject_agent_dock))
        .layer(axum::middleware::from_fn_with_state(
            store.clone(),
            require_login,
        ))
        .layer(CookieManagerLayer::new())
        // Outermost so the headers also land on auth redirects and static
        // assets, not just handler-rendered pages.
        .layer(axum::middleware::from_fn(security_headers));

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

async fn setup_get(State(ctx): State<DashboardCtx>) -> Response {
    if ctx.store.is_initialized() {
        return Redirect::to("/admin/").into_response();
    }
    SetupPage { error: None }.into_response()
}

async fn setup_post(
    State(ctx): State<DashboardCtx>,
    cookies: Cookies,
    Form(form): Form<SetupForm>,
) -> Response {
    if ctx.store.is_initialized() {
        return Redirect::to("/admin/").into_response();
    }

    if form.password != form.confirm {
        return SetupPage {
            error: Some("Passwords do not match."),
        }
        .into_response();
    }

    match ctx.store.create_admin(&form.username, &form.password) {
        Ok(()) => {
            // The store trims the username on write; the cookie must
            // carry the same canonical form.
            issue_session_cookie(&ctx.store, &cookies, form.username.trim());
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

async fn login_get(State(ctx): State<DashboardCtx>, cookies: Cookies) -> Response {
    // Auth switched off — the form is pointless, go straight in.
    if ctx.store.auth_disabled {
        return Redirect::to("/admin/").into_response();
    }
    if !ctx.store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }
    // Already signed in — skip the form and land on the dashboard.
    if session_is_valid(&ctx.store, &cookies) {
        return Redirect::to("/admin/").into_response();
    }
    LoginPage {
        error: None,
        username: "",
    }
    .into_response()
}

async fn login_post(
    State(ctx): State<DashboardCtx>,
    cookies: Cookies,
    Form(form): Form<LoginForm>,
) -> Response {
    if !ctx.store.is_initialized() {
        return Redirect::to("/admin/setup").into_response();
    }

    if ctx.store.verify_login(&form.username, &form.password) {
        issue_session_cookie(&ctx.store, &cookies, form.username.trim());
        Redirect::to("/admin/").into_response()
    } else {
        // Re-render with the username preserved so the operator only has
        // to retype the password. Deliberately vague message — don't leak
        // which half was wrong.
        LoginPage {
            error: Some("Invalid username or password."),
            username: &form.username,
        }
        .into_response()
    }
}

// ---------- Logout ----------

async fn logout(State(ctx): State<DashboardCtx>, cookies: Cookies) -> Response {
    let key = tower_cookies::Key::from(&ctx.store.cookie_key);
    let signed = cookies.signed(&key);
    // The removal cookie must carry the same path the session was issued
    // with, or the browser keeps the original alive.
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/admin");
    signed.remove(cookie);
    Redirect::to("/admin/login").into_response()
}

// ---------- Dashboard ----------

async fn dashboard(State(ctx): State<DashboardCtx>) -> Response {
    let username = match ctx.store.admin() {
        Some(a) => a.username,
        None => "admin".to_string(),
    };

    DashboardPage {
        admin_username: username,
    }
    .into_response()
}

// ---------- API: /admin/api/plugins ----------

async fn api_plugins(State(ctx): State<DashboardCtx>) -> Response {
    let rows = ctx.plugin_manager.rail_rows(&ctx.app_ctx);
    Json(rows).into_response()
}

async fn api_plugin_status(State(ctx): State<DashboardCtx>, Path(id): Path<String>) -> Response {
    match ctx.plugin_manager.plugin_status(&ctx.app_ctx, &id) {
        Some(status) => Json(status).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------- Public assets (mounted via `CmsPlugin::routes`, NOT /admin) ----------

/// Router for the CMS plugin's *public* surface. Currently just the
/// shared stylesheet that listener-facing pages (radio player, etc.)
/// link. Merged into the host's root router by `PluginManager`, so no
/// auth middleware applies here — everything on it must be safe to serve
/// anonymously.
pub fn build_public_routes() -> Router {
    Router::new().route("/assets/public.css", get(serve_public_css))
}

/// Serve `static/public.css` with a short public cache. The file is
/// authored by the front-end lane and may not exist yet — a missing file
/// is a plain 404, not an error.
async fn serve_public_css() -> Response {
    let path = static_assets_dir().join("public.css");
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------- Helpers ----------

fn issue_session_cookie(store: &AdminStore, cookies: &Cookies, username: &str) {
    let key = tower_cookies::Key::from(&store.cookie_key);
    let signed = cookies.signed(&key);
    let mut cookie = Cookie::new(SESSION_COOKIE, session_value(username));
    cookie.set_path("/admin");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    signed.add(cookie);
}
