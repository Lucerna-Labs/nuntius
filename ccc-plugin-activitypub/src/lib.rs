//! ActivityPub plugin: real, signed federation.
//!
//! Public surface:
//! - WebFinger: `GET /.well-known/webfinger?resource=acct:user@host`
//! - Actor:     `GET /users/:username` returns ActivityStreams 2.0 actor JSON
//!   with a real RSA-2048 publicKey PEM.
//! - Outbox:    `GET /users/:username/outbox` — published blog posts as Notes
//! - Inbox:     `POST /users/:username/inbox` — parses Follow / Undo Follow.
//!   On Follow we fetch the remote actor, persist the follower in redb,
//!   and asynchronously deliver a signed Accept back to their inbox.
//!
//! Outbound delivery (cross-plugin):
//! - On `Event::BlogPostPublished` and `Event::PodcastEpisodePublished`,
//!   we build a Create Note activity and fan it out (HTTP-Signature signed
//!   per spec draft-cavage v10) to every follower's inbox.
//!
//! Admin surface:
//! - `/admin/federation` — handle/bio editor + live follower count.
//!
//! Keypair: 2048-bit RSA generated on first boot (one-time) and stored in
//! `federation.toml` (PEM). Form-driven settings updates preserve key
//! material so editing the bio doesn't rotate keys.
//!
//! NOT yet (follow-up): verifying HTTP signatures on incoming requests;
//! handling Move / Delete activities; backfilling shared inbox routing.

mod sig;

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_database::{CccDB, FederationFollower, PostStatus};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn generate_keypair_pem() -> Result<(String, String), String> {
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| e.to_string())?;
    let public_key = RsaPublicKey::from(&private_key);
    let priv_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    let pub_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    Ok((priv_pem, pub_pem))
}

// ============================================================================
// Settings (federation.toml sidecar)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationSettings {
    pub display_name: String,
    pub username: String,
    pub host: String,
    pub summary: String,
    /// PEM-encoded RSA public key. Generated on first boot; embedded in
    /// the Actor JSON so other Fediverse servers can verify our signed
    /// activities. Empty string until first boot writes one in.
    #[serde(default)]
    pub public_key_pem: String,
    /// PEM-encoded RSA private key. Stays on the server, never sent over
    /// the wire. Used to sign outbound HTTP requests + activities.
    #[serde(default)]
    pub private_key_pem: String,
}

impl Default for FederationSettings {
    fn default() -> Self {
        Self {
            display_name: "Nuntius".to_string(),
            username: "station".to_string(),
            host: "ccc.example.com".to_string(),
            summary: "The official broadcast feed.".to_string(),
            public_key_pem: String::new(),
            private_key_pem: String::new(),
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
        let mut federation = if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(io_err)?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.federation
        } else {
            FederationSettings::default()
        };

        // Generate a real RSA keypair the first time we see this sidecar
        // file with no key material. 2048 is the AP de-facto floor.
        if federation.private_key_pem.is_empty() || federation.public_key_pem.is_empty() {
            println!("[federation] generating RSA-2048 keypair (one-time)...");
            let (priv_pem, pub_pem) = generate_keypair_pem()
                .map_err(|e| PluginError::InitError(format!("RSA keygen failed: {}", e)))?;
            federation.private_key_pem = priv_pem;
            federation.public_key_pem = pub_pem;
        }

        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(federation),
        };
        // Persist if the file didn't exist OR the keypair was just generated.
        store.persist()?;
        Ok(store)
    }

    pub fn snapshot(&self) -> FederationSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, mut new: FederationSettings) -> Result<(), PluginError> {
        // Form posts don't carry the key material — keep what we have on
        // disk if the new payload omits it.
        let prev = self.snapshot();
        if new.private_key_pem.is_empty() {
            new.private_key_pem = prev.private_key_pem;
        }
        if new.public_key_pem.is_empty() {
            new.public_key_pem = prev.public_key_pem;
        }
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { federation: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
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
    /// One-shot guard for the publish-listener background task.
    /// Replaces the old `static SPAWNED: AtomicBool` pattern that
    /// didn't work if you ever constructed two plugin instances.
    publish_listener_spawned: AtomicBool,
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
            publish_listener_spawned: AtomicBool::new(false),
        })
    }
}

#[derive(Clone)]
struct AppState {
    settings: Arc<SettingsStore>,
    db: Arc<CccDB>,
}

#[derive(Clone)]
struct AdminState {
    settings: Arc<SettingsStore>,
    db: Arc<CccDB>,
}

impl Plugin for CccPluginActivitypubPlugin {
    fn name(&self) -> &'static str {
        "Federation"
    }

    fn version(&self) -> &'static str {
        "0.3.0"
    }

    fn id(&self) -> String {
        "activitypub".into()
    }
    fn kind(&self) -> &'static str {
        "ACTIVITYPUB"
    }
    fn icon(&self) -> &'static str {
        "icon-fed"
    }
    fn admin_url(&self) -> String {
        "/admin/federation".into()
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let snap = self.settings.snapshot();
        let followers = ctx.db.all_followers().unwrap_or_default();
        let host_warning = snap.host.contains("example.com");
        PluginStatus {
            state: if snap.private_key_pem.is_empty() || host_warning {
                PluginLifecycle::Unconfigured
            } else {
                PluginLifecycle::Configured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": !snap.private_key_pem.is_empty() && !host_warning,
                "actor": {
                    "display_name": snap.display_name,
                    "username": snap.username,
                    "host": snap.host,
                    "summary": snap.summary,
                    "followers": followers.len(),
                    "following": 0u64,
                    "host_warning": if host_warning {
                        Some(format!("Host is still set to {} — update before public launch.", snap.host))
                    } else { None },
                },
                "inbox": Vec::<serde_json::Value>::new(),
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        let snap = self.settings.snapshot();
        println!(
            "Initializing Federation: handle=@{}@{} (RSA keypair {})",
            snap.username,
            snap.host,
            if snap.private_key_pem.is_empty() {
                "MISSING"
            } else {
                "ready"
            }
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

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let admin_state = AdminState {
            settings: self.settings.clone(),
            db: ctx.db.clone(),
        };
        // Subscribe to publish events (blog/podcast) and fan out
        // Create Note activities to followers.
        spawn_publish_listener(
            self.settings.clone(),
            ctx.db.clone(),
            ctx.events.clone(),
            &self.publish_listener_spawned,
        );
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

async fn inbox(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let snap = state.settings.snapshot();
    if username != snap.username {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Verify the HTTP signature if one is present. We require it for
    // mutating activities (Follow / Undo / Move / Delete) — anonymous
    // POSTs to the inbox are rejected.
    let target_path = format!("/users/{}/inbox", snap.username);
    if let Err(e) = verify_inbound(&headers, &body, "post", &target_path).await {
        eprintln!("[federation] inbox signature rejected: {}", e);
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let activity: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let activity_type = activity["type"].as_str().unwrap_or("").to_string();
    let actor_url_in = activity["actor"].as_str().unwrap_or("").to_string();
    println!(
        "[federation] inbox: type={} actor={}",
        activity_type, actor_url_in
    );

    match activity_type.as_str() {
        "Follow" => {
            let db = state.db.clone();
            let settings = state.settings.clone();
            let activity_clone = activity.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_follow(&db, &settings, activity_clone).await {
                    eprintln!("[federation] follow handler failed: {}", e);
                }
            });
        }
        "Undo" => {
            // Undo Follow: drop the follower row.
            if activity["object"]["type"].as_str() == Some("Follow") {
                let _ = state.db.delete_follower(&actor_url_in);
                println!("[federation] follower removed: {}", actor_url_in);
            }
        }
        "Delete" => {
            // Account deletion: if an actor we know is being deleted,
            // remove them from our followers list.
            let target = activity["object"]
                .as_str()
                .or_else(|| activity["object"]["id"].as_str())
                .unwrap_or("");
            if !target.is_empty() {
                let _ = state.db.delete_follower(target);
                println!("[federation] delete: {} (if follower)", target);
            }
        }
        "Move" => {
            // Account migration: AP servers send Move {actor, target}.
            // We re-key the follower row to the new actor URL by
            // inserting fresh and deleting the old.
            let new_actor = activity["target"].as_str().unwrap_or("").to_string();
            if !new_actor.is_empty() && !actor_url_in.is_empty() {
                if let Ok(followers) = state.db.all_followers() {
                    if let Some(prev) = followers.into_iter().find(|f| f.actor_url == actor_url_in)
                    {
                        let mut migrated = prev.clone();
                        migrated.actor_url = new_actor.clone();
                        let _ = state.db.upsert_follower(&migrated);
                        let _ = state.db.delete_follower(&actor_url_in);
                        println!(
                            "[federation] follower migrated: {} -> {}",
                            actor_url_in, new_actor
                        );
                    }
                }
            }
        }
        _ => {
            // Anything else: accept silently. Like, Announce, Create
            // from someone else, etc. — we don't store remote content yet.
        }
    }

    StatusCode::ACCEPTED.into_response()
}

/// Verify the `Signature` header against `body` using the public key at
/// the actor URL named in `keyId`. Returns Err with a human-readable
/// reason if anything goes wrong; the inbox handler logs and 401s.
async fn verify_inbound(
    headers: &axum::http::HeaderMap,
    body: &[u8],
    method: &str,
    target_path: &str,
) -> Result<(), String> {
    let sig_header = headers
        .get("signature")
        .ok_or_else(|| "missing Signature header".to_string())?
        .to_str()
        .map_err(|_| "Signature header not ASCII".to_string())?;
    let parsed = sig::parse_signature_header(sig_header)
        .ok_or_else(|| "could not parse Signature header".to_string())?;

    // Sanity-check the digest matches the body, if it's claimed in headers.
    if parsed.headers.iter().any(|h| h == "digest") {
        let provided = headers
            .get("digest")
            .ok_or("digest claimed in signature but missing from request")?
            .to_str()
            .map_err(|_| "Digest header not ASCII")?;
        let computed = sig::body_digest(body);
        if provided != computed {
            return Err("digest header does not match body hash".to_string());
        }
    }

    // Fetch the remote actor's public key.
    let key_url = parsed.key_id.split('#').next().unwrap_or(&parsed.key_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let actor: serde_json::Value = client
        .get(key_url)
        .header("Accept", "application/activity+json")
        .send()
        .await
        .map_err(|e| format!("fetch actor: {}", e))?
        .json()
        .await
        .map_err(|e| format!("parse actor JSON: {}", e))?;
    let pub_pem = actor["publicKey"]["publicKeyPem"]
        .as_str()
        .ok_or("remote actor has no publicKeyPem")?;

    let signing_string = sig::build_signing_string(&parsed.headers, method, target_path, headers)?;
    sig::verify_rsa_sha256(pub_pem, &signing_string, &parsed.signature_b64)
}

/// Persist the follower, fetch their inbox URL from their actor profile,
/// then deliver a signed `Accept` activity back to that inbox.
async fn handle_follow(
    db: &CccDB,
    settings: &SettingsStore,
    activity: serde_json::Value,
) -> Result<(), String> {
    let snap = settings.snapshot();
    let actor_url_in = activity["actor"]
        .as_str()
        .ok_or_else(|| "no actor".to_string())?
        .to_string();
    let activity_id = activity["id"].as_str().unwrap_or("").to_string();

    // Fetch the remote actor to learn their inbox URL.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&actor_url_in)
        .header("Accept", "application/activity+json")
        .send()
        .await
        .map_err(|e| format!("fetch actor failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("actor fetch returned {}", resp.status()));
    }
    let actor_json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let inbox_url = actor_json["inbox"]
        .as_str()
        .ok_or_else(|| "remote actor has no inbox".to_string())?
        .to_string();
    let preferred_username = actor_json["preferredUsername"]
        .as_str()
        .map(|s| s.to_string());
    let display_name = actor_json["name"].as_str().map(|s| s.to_string());

    // Persist the follower.
    let follower = FederationFollower {
        actor_url: actor_url_in.clone(),
        inbox_url: inbox_url.clone(),
        preferred_username,
        name: display_name,
        followed_at: unix_now(),
    };
    db.upsert_follower(&follower)
        .map_err(|e| format!("persist follower: {}", e))?;
    println!("[federation] new follower: {}", actor_url_in);

    // Build the Accept activity and deliver it back.
    let our_actor = actor_url(&snap);
    let accept = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}/activities/accept-{}", our_actor, unix_now()),
        "type": "Accept",
        "actor": our_actor,
        "object": activity,
    });
    let _ = activity_id; // kept in case we want to log
    deliver_signed(&snap, &inbox_url, &accept).await
}

/// POST `body` (an ActivityStreams object) to `inbox_url` with an
/// HTTP-Signature header. Returns Err with the response body on failure.
async fn deliver_signed(
    snap: &FederationSettings,
    inbox_url: &str,
    body: &serde_json::Value,
) -> Result<(), String> {
    let body_bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let parsed = url::Url::parse(inbox_url).map_err(|e| format!("bad inbox URL: {}", e))?;
    let host = parsed.host_str().ok_or("no host in inbox URL")?.to_string();
    let path = if parsed.query().is_some() {
        format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""))
    } else {
        parsed.path().to_string()
    };

    let date = sig::http_date(unix_now());
    let digest = sig::body_digest(&body_bytes);
    let key_id = format!("{}#main-key", actor_url(snap));
    let signature = sig::sign_post(&snap.private_key_pem, &key_id, &path, &host, &date, &digest)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(inbox_url)
        .header("Host", &host)
        .header("Date", &date)
        .header("Digest", &digest)
        .header("Signature", &signature)
        .header("Content-Type", "application/activity+json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| format!("delivery failed: {}", e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "delivery {} returned {}: {}",
            inbox_url, status, text
        ));
    }
    println!("[federation] delivered to {}", inbox_url);
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Subscribe to BlogPost / Podcast publish events; on each, fan out a
/// Create Note activity to every follower's inbox.
fn spawn_publish_listener(
    settings: Arc<SettingsStore>,
    db: Arc<CccDB>,
    events: Arc<EventBus>,
    spawned: &AtomicBool,
) {
    // Per-plugin one-shot guard. Replaces the global `static SPAWNED`
    // pattern that would prevent two plugin instances from each
    // subscribing (a real concern for tests and hot-reload).
    if spawned.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut rx = events.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match rx.recv().await {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            match event {
                Event::BlogPostPublished { id } => fan_out_blog(&settings, &db, id).await,
                Event::PodcastEpisodePublished { id } => fan_out_podcast(&settings, &db, id).await,
                _ => {}
            }
        }
    });
}

async fn fan_out_blog(settings: &SettingsStore, db: &CccDB, post_id: u64) {
    let post = match db.get_blog_post(post_id) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        _ => return,
    };
    let snap = settings.snapshot();
    let actor = actor_url(&snap);
    let post_url = format!("https://{}/blog/{}", snap.host, post.slug);
    let note = serde_json::json!({
        "id": format!("{}/posts/blog-{}", actor, post.id),
        "type": "Note",
        "attributedTo": actor,
        "url": post_url,
        "content": format!("<p><a href=\"{}\">{}</a></p>", post_url, post.title),
        "published": iso8601_utc(post.published_at.unwrap_or(post.updated_at)),
    });
    let create = create_activity(&snap, &note);
    deliver_to_all_followers(db, &snap, &create).await;
}

async fn fan_out_podcast(settings: &SettingsStore, db: &CccDB, ep_id: u64) {
    let ep = match db.get_podcast_episode(ep_id) {
        Ok(Some(e)) => e,
        _ => return,
    };
    let snap = settings.snapshot();
    let actor = actor_url(&snap);
    let ep_url = format!("https://{}/podcasts/{}", snap.host, ep.slug);
    let note = serde_json::json!({
        "id": format!("{}/posts/podcast-{}", actor, ep.id),
        "type": "Note",
        "attributedTo": actor,
        "url": ep_url,
        "content": format!("<p>New episode: <a href=\"{}\">{}</a></p>", ep_url, ep.title),
        "published": iso8601_utc(ep.published_at.unwrap_or(ep.updated_at)),
    });
    let create = create_activity(&snap, &note);
    deliver_to_all_followers(db, &snap, &create).await;
}

fn create_activity(snap: &FederationSettings, note: &serde_json::Value) -> serde_json::Value {
    let actor = actor_url(snap);
    serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}/activities/create-{}", actor, unix_now()),
        "type": "Create",
        "actor": actor,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "cc": [format!("{}/followers", actor)],
        "object": note,
    })
}

async fn deliver_to_all_followers(
    db: &CccDB,
    snap: &FederationSettings,
    activity: &serde_json::Value,
) {
    let followers = match db.all_followers() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[federation] could not load followers: {}", e);
            return;
        }
    };
    if followers.is_empty() {
        return;
    }
    println!(
        "[federation] fanning out activity to {} follower(s)",
        followers.len()
    );
    for f in followers {
        if let Err(e) = deliver_signed(snap, &f.inbox_url, activity).await {
            eprintln!("[federation] delivery to {} failed: {}", f.inbox_url, e);
        }
    }
}

fn actor_url(snap: &FederationSettings) -> String {
    format!("https://{}/users/{}", snap.host, snap.username)
}

/// Minimal RFC 3339 / ISO 8601 UTC formatter for ActivityPub `published` fields.
fn iso8601_utc(secs: u64) -> String {
    let (y, mo, d, h, mi) = unix_to_ymdhm(secs);
    let s = (secs % 60) as u32;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
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
    let months = if is_leap(year) {
        months_leap
    } else {
        months_normal
    };
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

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage<'a> {
    display_name: String,
    username: String,
    host: String,
    summary: String,
    follower_count: usize,
    error: Option<&'a str>,
    saved: Option<bool>,
}

async fn admin_get(
    State(state): State<AdminState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let snap = state.settings.snapshot();
    let follower_count = state.db.all_followers().map(|v| v.len()).unwrap_or(0);
    render(AdminSettingsPage {
        display_name: snap.display_name,
        username: snap.username,
        host: snap.host,
        summary: snap.summary,
        follower_count,
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
    let follower_count = state.db.all_followers().map(|v| v.len()).unwrap_or(0);
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
            follower_count,
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
            follower_count,
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
