//! Patreon plugin: OAuth code-flow login, identity fetch, signed
//! session cookie, tier-aware membership lookup for sibling plugins.
//!
//! Public:
//! - GET /patreon            — status page (login button or patron info)
//! - GET /patreon/login      — 302 → Patreon authorize URL
//! - GET /patreon/callback   — exchanges ?code= for an access token,
//!   fetches identity + memberships, sets cookie
//! - GET /patreon/logout     — clears cookie
//!
//! Admin:
//! - GET/POST /admin/patreon — credential settings + required_tier_id
//!
//! Sibling-plugin helper (resolved via `manager.find::<...>`):
//! - `current_patron(&cookies) -> Option<PatronSession>`
//! - `is_member(&cookies) -> bool`

use askama::Template;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tower_cookies::{Cookie, CookieManagerLayer, Cookies, Key};

const PATREON_AUTHORIZE: &str = "https://www.patreon.com/oauth2/authorize";
const PATREON_TOKEN: &str = "https://www.patreon.com/api/oauth2/token";
const PATREON_IDENTITY: &str =
    "https://www.patreon.com/api/oauth2/v2/identity?include=memberships.currently_entitled_tiers&fields%5Bmember%5D=patron_status,currently_entitled_amount_cents&fields%5Buser%5D=full_name";

const SESSION_COOKIE: &str = "ccc_patreon";

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatreonSettings {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub required_tier_id: String,
    /// Hex-encoded 64-byte cookie key. Generated on first boot.
    #[serde(default)]
    pub cookie_key_hex: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    patreon: PatreonSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<PatreonSettings>,
    cookie_key: Vec<u8>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let mut p = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| PluginError::InitError(format!("patreon IO: {}", e)))?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.patreon
        } else {
            PatreonSettings::default()
        };
        if p.cookie_key_hex.is_empty() {
            let mut bytes = [0u8; 64];
            rand::thread_rng().fill_bytes(&mut bytes);
            p.cookie_key_hex = hex_encode(&bytes);
        }
        let cookie_key = hex_decode(&p.cookie_key_hex).ok_or_else(|| {
            PluginError::InitError("patreon cookie_key_hex invalid".into())
        })?;
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(p),
            cookie_key,
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> PatreonSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, mut new: PatreonSettings) -> Result<(), PluginError> {
        if new.cookie_key_hex.is_empty() {
            new.cookie_key_hex = hex_encode(&self.cookie_key);
        }
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { patreon: snap };
        let raw = toml::to_string_pretty(&file)
            .map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PluginError::InitError(format!("patreon IO: {}", e)))?;
            }
        }
        std::fs::write(&self.path, raw)
            .map_err(|e| PluginError::InitError(format!("patreon IO: {}", e)))?;
        Ok(())
    }
}

fn hex_encode(b: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{:02x}", x);
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(pair, 16).ok()?);
    }
    Some(out)
}

// ============================================================================
// Public types
// ============================================================================

/// What we cookie back to the browser after a successful Patreon login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatronSession {
    pub patron_id: String,
    pub full_name: String,
    pub is_member: bool,
    /// Unix seconds.
    pub issued_at: u64,
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginPatreonPlugin {
    settings: Arc<SettingsStore>,
}

impl CccPluginPatreonPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("patreon")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("patreon.toml"));
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        Ok(Self { settings: store })
    }

    pub fn is_configured(&self) -> bool {
        let s = self.settings.snapshot();
        !s.client_id.is_empty() && !s.client_secret.is_empty() && !s.redirect_uri.is_empty()
    }

    /// Decode the patron session cookie, if present and valid.
    pub fn current_patron(&self, cookies: &Cookies) -> Option<PatronSession> {
        let key = Key::from(&self.settings.cookie_key);
        let signed = cookies.signed(&key);
        let raw = signed.get(SESSION_COOKIE)?;
        serde_json::from_str(raw.value()).ok()
    }

    /// True iff the cookie indicates an active member at the configured tier.
    pub fn is_member(&self, cookies: &Cookies) -> bool {
        match self.current_patron(cookies) {
            Some(s) => s.is_member,
            None => false,
        }
    }
}

#[derive(Clone)]
struct PluginState {
    settings: Arc<SettingsStore>,
}

impl Plugin for CccPluginPatreonPlugin {
    fn name(&self) -> &'static str {
        "Patreon"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let configured = self.is_configured();
        println!("Initializing Patreon: configured={}", configured);
        Ok(())
    }

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        let state = PluginState {
            settings: self.settings.clone(),
        };
        Some(
            Router::new()
                .route("/patreon", get(public_status))
                .route("/patreon/login", get(public_login))
                .route("/patreon/callback", get(public_callback))
                .route("/patreon/logout", get(public_logout))
                .with_state(state)
                .layer(CookieManagerLayer::new()),
        )
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/patreon", get(admin_get).post(admin_post))
                .with_state(PluginState {
                    settings: self.settings.clone(),
                }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Patreon",
            href: "/admin/patreon".into(),
            icon: "❤️",
            sort: 800,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Public handlers
// ============================================================================

#[derive(Template)]
#[template(path = "public_status.html")]
struct PublicStatusPage<'a> {
    patron: Option<PatronSession>,
    configured: bool,
    error: Option<&'a str>,
}

async fn public_status(State(state): State<PluginState>, cookies: Cookies) -> Response {
    let key = Key::from(&state.settings.cookie_key);
    let signed = cookies.signed(&key);
    let patron = signed
        .get(SESSION_COOKIE)
        .and_then(|c| serde_json::from_str::<PatronSession>(c.value()).ok());
    let configured = !state.settings.snapshot().client_id.is_empty();
    render(PublicStatusPage {
        patron,
        configured,
        error: None,
    })
}

async fn public_login(State(state): State<PluginState>) -> Response {
    let s = state.settings.snapshot();
    if s.client_id.is_empty() || s.redirect_uri.is_empty() {
        return render(PublicStatusPage {
            patron: None,
            configured: false,
            error: Some("Patreon isn't configured."),
        });
    }
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope=identity%20identity.memberships",
        PATREON_AUTHORIZE,
        url::form_urlencoded::byte_serialize(s.client_id.as_bytes()).collect::<String>(),
        url::form_urlencoded::byte_serialize(s.redirect_uri.as_bytes()).collect::<String>()
    );
    Redirect::temporary(&url).into_response()
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
}

async fn public_callback(
    State(state): State<PluginState>,
    cookies: Cookies,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if let Some(err) = q.error {
        return render(PublicStatusPage {
            patron: None,
            configured: true,
            error: Some(&format!("Patreon returned error: {}", err)),
        });
    }
    let code = match q.code {
        Some(c) => c,
        None => {
            return render(PublicStatusPage {
                patron: None,
                configured: true,
                error: Some("Patreon callback missing code."),
            });
        }
    };
    let s = state.settings.snapshot();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return error_page(&format!("HTTP client init failed: {}", e)),
    };

    // Exchange code for token.
    let token_form = [
        ("code", code.as_str()),
        ("grant_type", "authorization_code"),
        ("client_id", &s.client_id),
        ("client_secret", &s.client_secret),
        ("redirect_uri", &s.redirect_uri),
    ];
    let token_resp = match client.post(PATREON_TOKEN).form(&token_form).send().await {
        Ok(r) => r,
        Err(e) => return error_page(&format!("Token request failed: {}", e)),
    };
    if !token_resp.status().is_success() {
        let body = token_resp.text().await.unwrap_or_default();
        return error_page(&format!("Token exchange rejected: {}", body));
    }
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
    }
    let token: TokenResp = match token_resp.json().await {
        Ok(v) => v,
        Err(e) => return error_page(&format!("Token JSON parse failed: {}", e)),
    };

    // Fetch identity + memberships.
    let identity_resp = match client
        .get(PATREON_IDENTITY)
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return error_page(&format!("Identity request failed: {}", e)),
    };
    let identity_json: serde_json::Value = match identity_resp.json().await {
        Ok(v) => v,
        Err(e) => return error_page(&format!("Identity JSON parse failed: {}", e)),
    };

    let patron_id = identity_json["data"]["id"].as_str().unwrap_or("").to_string();
    let full_name = identity_json["data"]["attributes"]["full_name"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let is_member = membership_check(&identity_json, &s.required_tier_id);

    let session = PatronSession {
        patron_id,
        full_name,
        is_member,
        issued_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let json = match serde_json::to_string(&session) {
        Ok(j) => j,
        Err(e) => return error_page(&format!("Session serialize failed: {}", e)),
    };

    let key = Key::from(&state.settings.cookie_key);
    let signed = cookies.signed(&key);
    let mut cookie = Cookie::new(SESSION_COOKIE, json);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::days(30));
    signed.add(cookie);

    Redirect::to("/patreon").into_response()
}

fn membership_check(identity: &serde_json::Value, required_tier: &str) -> bool {
    let included = match identity.get("included").and_then(|v| v.as_array()) {
        Some(v) => v,
        None => return false,
    };
    let mut active_member = false;
    let mut entitled_to_required = required_tier.is_empty();
    for item in included {
        if item["type"] == "member"
            && item["attributes"]["patron_status"]
                .as_str()
                .map(|s| s == "active_patron")
                .unwrap_or(false)
        {
            active_member = true;
            if !required_tier.is_empty() {
                if let Some(tiers) = item["relationships"]["currently_entitled_tiers"]["data"]
                    .as_array()
                {
                    for tier in tiers {
                        if tier["id"].as_str() == Some(required_tier) {
                            entitled_to_required = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    active_member && entitled_to_required
}

fn error_page(msg: &str) -> Response {
    render(PublicStatusPage {
        patron: None,
        configured: true,
        error: Some(msg),
    })
}

async fn public_logout(State(state): State<PluginState>, cookies: Cookies) -> Response {
    let key = Key::from(&state.settings.cookie_key);
    let signed = cookies.signed(&key);
    signed.remove(Cookie::new(SESSION_COOKIE, ""));
    Redirect::to("/patreon").into_response()
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    required_tier_id: String,
    configured: bool,
    saved: Option<bool>,
}

async fn admin_get(
    State(state): State<PluginState>,
    Query(_): Query<HashMap<String, String>>,
) -> Response {
    let s = state.settings.snapshot();
    let configured = !s.client_id.is_empty() && !s.client_secret.is_empty();
    render(AdminSettingsPage {
        client_id: s.client_id,
        client_secret: s.client_secret,
        redirect_uri: s.redirect_uri,
        required_tier_id: s.required_tier_id,
        configured,
        saved: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    required_tier_id: String,
}

async fn admin_post(State(state): State<PluginState>, Form(form): Form<SettingsForm>) -> Response {
    let new = PatreonSettings {
        client_id: form.client_id.trim().to_string(),
        client_secret: form.client_secret.trim().to_string(),
        redirect_uri: form.redirect_uri.trim().to_string(),
        required_tier_id: form.required_tier_id.trim().to_string(),
        cookie_key_hex: state.settings.snapshot().cookie_key_hex,
    };
    let _ = state.settings.replace(new);
    Redirect::to("/admin/patreon?saved=1").into_response()
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
