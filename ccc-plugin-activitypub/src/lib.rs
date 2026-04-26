//! ActivityPub plugin: makes the station discoverable on the Fediverse.
//!
//! v1 surface (preview-quality, no signed delivery yet):
//! - WebFinger: `GET /.well-known/webfinger?resource=acct:user@host`
//! - Actor:     `GET /users/:username` returns ActivityStreams 2.0 actor JSON
//! - Outbox:    `GET /users/:username/outbox` returns an OrderedCollection
//!   of the station's published blog posts as Notes
//! - Inbox:     `POST /users/:username/inbox` accepts and logs (no-op)
//!
//! Admin surface:
//! - `/admin/federation` — edit display name, username, public hostname,
//!   bio. Persisted in `federation.toml` next to `ccc.toml`.
//!
//! Real signed federation (RSA, HTTP Signatures, Accept Follow,
//! Create Note delivery) is intentionally a follow-up.

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use ccc_database::{CccDB, PostStatus};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, RwLock};

// ============================================================================
// Settings (federation.toml sidecar)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationSettings {
    pub display_name: String,
    pub username: String,
    pub host: String,
    pub summary: String,
    /// Placeholder PEM. Real RSA generation lands with signed delivery.
    #[serde(default = "default_public_key_pem")]
    pub public_key_pem: String,
}

fn default_public_key_pem() -> String {
    "-----BEGIN PUBLIC KEY-----\n(placeholder; real key generated when signed delivery ships)\n-----END PUBLIC KEY-----".to_string()
}

impl Default for FederationSettings {
    fn default() -> Self {
        Self {
            display_name: "Creator Command Center".to_string(),
            username: "station".to_string(),
            host: "ccc.example.com".to_string(),
            summary: "The official broadcast feed.".to_string(),
            public_key_pem: default_public_key_pem(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    federation: FederationSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<FederationSettings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &FsPath) -> Result<Self, PluginError> {
        let federation = if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(io_err)?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.federation
        } else {
            FederationSettings::default()
        };

        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(federation),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> FederationSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: FederationSettings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { federation: snap };
        let raw = toml::to_string_pretty(&file)
            .map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io_err)?;
            }
        }
        std::fs::write(&self.path, raw).map_err(io_err)?;
        Ok(())
    }
}

fn io_err(e: std::io::Error) -> PluginError {
    PluginError::InitError(format!("federation IO: {}", e))
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginActivitypubPlugin {
    settings: Arc<SettingsStore>,
}

impl CccPluginActivitypubPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("activitypub")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("federation.toml"));

        let store = SettingsStore::load_or_init(&path)?;
        Ok(Self {
            settings: Arc::new(store),
        })
    }
}

#[derive(Clone)]
struct AppState {
    settings: Arc<SettingsStore>,
    db: Arc<CccDB>,
}

impl Plugin for CccPluginActivitypubPlugin {
    fn name(&self) -> &'static str {
        "Federation (ActivityPub)"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let snap = self.settings.snapshot();
        println!(
            "Initializing Federation: handle=@{}@{}",
            snap.username, snap.host
        );
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AppState {
            settings: self.settings.clone(),
            db: ctx.db.clone(),
        };
        Some(
            Router::new()
                .route("/.well-known/webfinger", get(webfinger))
                .route("/users/:username", get(actor))
                .route("/users/:username/outbox", get(outbox))
                .route("/users/:username/inbox", post(inbox))
                .with_state(state),
        )
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        let admin_state = AdminState {
            settings: self.settings.clone(),
        };
        Some(
            Router::new()
                .route("/federation", get(admin_get).post(admin_post))
                .with_state(admin_state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Federation",
            href: "/admin/federation".into(),
            icon: "🌐",
            sort: 200,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Public ActivityPub handlers
// ============================================================================

async fn webfinger(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let resource = match params.get("resource") {
        Some(r) => r,
        None => return (StatusCode::BAD_REQUEST, "missing resource").into_response(),
    };
    let snap = state.settings.snapshot();
    let expected = format!("acct:{}@{}", snap.username, snap.host);
    if resource != &expected {
        return StatusCode::NOT_FOUND.into_response();
    }

    let actor_url = actor_url(&snap);
    let body = serde_json::json!({
        "subject": resource,
        "aliases": [actor_url],
        "links": [
            { "rel": "self", "type": "application/activity+json", "href": actor_url },
            { "rel": "http://webfinger.net/rel/profile-page", "type": "text/html", "href": actor_url },
        ]
    });
    (
        [(axum::http::header::CONTENT_TYPE, "application/jrd+json")],
        Json(body),
    )
        .into_response()
}

async fn actor(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let snap = state.settings.snapshot();
    if username != snap.username {
        return StatusCode::NOT_FOUND.into_response();
    }
    let actor_url = actor_url(&snap);
    let body = serde_json::json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1"
        ],
        "id": actor_url,
        "type": "Service",
        "preferredUsername": snap.username,
        "name": snap.display_name,
        "summary": snap.summary,
        "inbox": format!("{}/inbox", actor_url),
        "outbox": format!("{}/outbox", actor_url),
        "followers": format!("{}/followers", actor_url),
        "following": format!("{}/following", actor_url),
        "publicKey": {
            "id": format!("{}#main-key", actor_url),
            "owner": actor_url,
            "publicKeyPem": snap.public_key_pem,
        }
    });
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/activity+json; charset=utf-8",
        )],
        Json(body),
    )
        .into_response()
}

async fn outbox(State(state): State<AppState>, Path(username): Path<String>) -> Response {
    let snap = state.settings.snapshot();
    if username != snap.username {
        return StatusCode::NOT_FOUND.into_response();
    }
    let actor_url_str = actor_url(&snap);
    let posts = state
        .db
        .latest_published_blog_posts(20)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.status == PostStatus::Published)
        .collect::<Vec<_>>();

    let items: Vec<serde_json::Value> = posts
        .iter()
        .map(|p| {
            let post_url = format!("https://{}/blog/{}", snap.host, p.slug);
            serde_json::json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "id": format!("{}/posts/{}", actor_url_str, p.id),
                "type": "Create",
                "actor": actor_url_str,
                "published": iso8601_utc(p.published_at.unwrap_or(p.updated_at)),
                "object": {
                    "id": post_url,
                    "type": "Note",
                    "attributedTo": actor_url_str,
                    "url": post_url,
                    "content": format!("<a href=\"{}\">{}</a>", post_url, p.title),
                    "published": iso8601_utc(p.published_at.unwrap_or(p.updated_at)),
                }
            })
        })
        .collect();

    let body = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}/outbox", actor_url_str),
        "type": "OrderedCollection",
        "totalItems": items.len(),
        "orderedItems": items,
    });
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/activity+json; charset=utf-8",
        )],
        Json(body),
    )
        .into_response()
}

async fn inbox(Path(username): Path<String>) -> Response {
    // v1: accept everything, do nothing. Real Follow handling needs HTTP
    // Signature verification + outbound Accept delivery.
    println!("[federation] inbox hit for user '{}'", username);
    StatusCode::ACCEPTED.into_response()
}

fn actor_url(snap: &FederationSettings) -> String {
    format!("https://{}/users/{}", snap.host, snap.username)
}

/// Minimal RFC 3339 / ISO 8601 UTC formatter for ActivityPub `published` fields.
fn iso8601_utc(secs: u64) -> String {
    let (y, mo, d, h, mi) = unix_to_ymdhm(secs);
    let s = (secs % 60) as u32;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, mo, d, h, mi, s
    )
}

fn unix_to_ymdhm(secs: u64) -> (u32, u32, u32, u32, u32) {
    let minutes_total = secs / 60;
    let hours_total = minutes_total / 60;
    let days_since_epoch = (hours_total / 24) as u32;
    let minute = (minutes_total % 60) as u32;
    let hour = (hours_total % 24) as u32;

    let mut year = 1970u32;
    let mut days_remaining = days_since_epoch;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if days_remaining < in_year {
            break;
        }
        days_remaining -= in_year;
        year += 1;
    }

    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months_leap = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = if is_leap(year) { months_leap } else { months_normal };
    let mut month = 1u32;
    for &len in &months {
        if days_remaining < len {
            break;
        }
        days_remaining -= len;
        month += 1;
    }
    let day = days_remaining + 1;
    (year, month, day, hour, minute)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Clone)]
struct AdminState {
    settings: Arc<SettingsStore>,
}

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage<'a> {
    display_name: String,
    username: String,
    host: String,
    summary: String,
    error: Option<&'a str>,
    saved: Option<bool>,
}

async fn admin_get(
    State(state): State<AdminState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let snap = state.settings.snapshot();
    render(AdminSettingsPage {
        display_name: snap.display_name,
        username: snap.username,
        host: snap.host,
        summary: snap.summary,
        error: None,
        saved: params.get("saved").map(|_| true),
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    display_name: String,
    username: String,
    host: String,
    summary: String,
}

async fn admin_post(State(state): State<AdminState>, Form(form): Form<SettingsForm>) -> Response {
    let trimmed_user = form.username.trim().to_string();
    if trimmed_user.is_empty()
        || !trimmed_user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return render(AdminSettingsPage {
            display_name: form.display_name,
            username: trimmed_user,
            host: form.host,
            summary: form.summary,
            error: Some("Username must be ASCII letters, digits, or underscores only."),
            saved: None,
        });
    }

    let mut snap = state.settings.snapshot();
    snap.display_name = form.display_name.trim().to_string();
    snap.username = trimmed_user;
    snap.host = form.host.trim().to_string();
    snap.summary = form.summary;

    if let Err(e) = state.settings.replace(snap.clone()) {
        return render(AdminSettingsPage {
            display_name: snap.display_name,
            username: snap.username,
            host: snap.host,
            summary: snap.summary,
            error: Some(&format!("Could not save: {}", e)),
            saved: None,
        });
    }
    Redirect::to("/admin/federation?saved=1").into_response()
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
