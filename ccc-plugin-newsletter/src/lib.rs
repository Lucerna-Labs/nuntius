//! Newsletter plugin: subscriber list + SMTP delivery + admin compose.
//!
//! Public:
//! - GET  /newsletter/subscribe — sign-up form
//! - POST /newsletter/subscribe — adds subscriber (no double opt-in for v1)
//!
//! Admin:
//! - GET  /admin/newsletter           — subscriber list
//! - POST /admin/newsletter/:id/delete
//! - GET  /admin/newsletter/compose   — compose form
//! - POST /admin/newsletter/send      — send to all active subscribers via SMTP
//! - GET/POST /admin/newsletter/settings — SMTP credentials

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_database::{CccDB, PostStatus, Subscriber};
use ccc_plugin_utils::{format_unix_utc, render, unix_now};
use lettre::message::{header::ContentType, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsletterSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from_email: String,
    pub from_name: String,
    /// When true, automatically email all active subscribers when a
    /// blog post transitions Draft -> Published.
    #[serde(default)]
    pub auto_send_blog_posts: bool,
    /// Public site URL used to build "Read more" links in auto-sent
    /// emails. Falls back to `Reading on the web` if blank.
    #[serde(default)]
    pub site_url: String,
}

impl Default for NewsletterSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 587,
            username: String::new(),
            password: String::new(),
            from_email: String::new(),
            // Deliberately the platform name: this Default is the serde
            // fallback for the TOML sidecar and cannot see ccc.toml. The
            // literal "Nuntius" acts as an "operator never touched this"
            // sentinel — at send time it is swapped for the configured
            // `[site] name` (see `effective_from_name`), never persisted.
            from_name: "Nuntius".into(),
            auto_send_blog_posts: false,
            site_url: String::new(),
        }
    }
}

/// Type alias preserving the historical name. The actual storage is
/// `TomlSidecar<NewsletterSettings>` from `ccc-plugin-utils`.
pub type SettingsStore = ccc_plugin_utils::TomlSidecar<NewsletterSettings>;

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginNewsletterPlugin {
    settings: Arc<SettingsStore>,
    /// Operator-configured site name (`[site] name` in ccc.toml). Brands
    /// the public subscribe page and substitutes for the untouched
    /// `from_name` default when sending (see `effective_from_name`).
    site_name: String,
}

impl CccPluginNewsletterPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("newsletter")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("newsletter.toml"));
        let store = Arc::new(
            SettingsStore::load_or_init(&path)
                .map_err(|e| PluginError::InitError(e.to_string()))?,
        );
        // `from_name` override: the sidecar's serde Default hard-codes
        // "Nuntius" because it cannot see ccc.toml. When the stored value
        // is still exactly that untouched default, the configured site
        // name is substituted at send time (`effective_from_name`) —
        // in memory only, never written back to newsletter.toml, so an
        // operator edit in the admin settings UI always wins. (The
        // TomlSidecar store has no non-persisting mutator, so the swap
        // happens at the point of use rather than here.)
        Ok(Self {
            settings: store,
            site_name: cfg.site.name.clone(),
        })
    }
}

impl Plugin for CccPluginNewsletterPlugin {
    fn name(&self) -> &'static str {
        "Newsletter"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn id(&self) -> String {
        "newsletter".into()
    }
    fn kind(&self) -> &'static str {
        "EMAIL"
    }
    fn icon(&self) -> &'static str {
        "icon-mail"
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let snap = self.settings.snapshot();
        let mut missing: Vec<&'static str> = Vec::new();
        if snap.host.is_empty() {
            missing.push("SMTP host");
        }
        if snap.username.is_empty() {
            missing.push("username");
        }
        if snap.password.is_empty() {
            missing.push("password");
        }
        if snap.from_email.is_empty() {
            missing.push("from_email");
        }
        let subs = ctx.db.all_subscribers().map(|v| v.len()).unwrap_or(0);
        let configured = missing.is_empty();
        PluginStatus {
            state: if configured {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": configured,
                "missing": missing,
                "file": "newsletter.toml",
                "auto_send_blog_posts": snap.auto_send_blog_posts,
                "subscribers": subs,
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        let snap = self.settings.snapshot();
        let configured = !snap.host.is_empty() && !snap.from_email.is_empty();
        println!(
            "Initializing Newsletter: host='{}' configured={}",
            snap.host, configured
        );
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route(
                    "/newsletter/subscribe",
                    get(public_subscribe_get).post(public_subscribe_post),
                )
                .with_state(PublicState {
                    db: ctx.db.clone(),
                    site_name: Arc::new(self.site_name.clone()),
                }),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let admin_state = AdminState {
            db: ctx.db.clone(),
            settings: self.settings.clone(),
            site_name: Arc::new(self.site_name.clone()),
        };
        // Spawn the cross-plugin event subscriber once, idempotently.
        spawn_event_listener(admin_state.clone(), ctx.events.clone());
        Some(
            Router::new()
                .route("/newsletter", get(admin_list))
                .route("/newsletter/compose", get(admin_compose_get))
                .route("/newsletter/send", post(admin_send))
                .route(
                    "/newsletter/settings",
                    get(admin_settings_get).post(admin_settings_post),
                )
                .route("/newsletter/:id/delete", post(admin_delete))
                .with_state(admin_state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Newsletter",
            href: "/admin/newsletter".into(),
            icon: "✉️",
            sort: 500,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Cross-plugin event listener (Blog -> auto newsletter)
// ============================================================================

fn spawn_event_listener(state: AdminState, events: Arc<EventBus>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SPAWNED: AtomicBool = AtomicBool::new(false);
    if SPAWNED.swap(true, Ordering::SeqCst) {
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
            if let Event::BlogPostPublished { id } = event {
                handle_blog_published(&state, id).await;
            }
        }
    });
}

async fn handle_blog_published(state: &AdminState, post_id: u64) {
    let snap = state.settings.snapshot();
    if !snap.auto_send_blog_posts {
        return;
    }
    if snap.host.is_empty() || snap.from_email.is_empty() {
        eprintln!(
            "[newsletter] auto-send skipped for blog post {}: SMTP not configured",
            post_id
        );
        return;
    }
    let post = match state.db.get_blog_post(post_id) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        Ok(_) => return,
        Err(e) => {
            eprintln!("[newsletter] could not load blog post {}: {}", post_id, e);
            return;
        }
    };
    let recipients: Vec<Subscriber> = state
        .db
        .all_subscribers()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.active)
        .collect();
    if recipients.is_empty() {
        return;
    }

    let mailer = match build_mailer(&snap) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[newsletter] auto-send mailer build failed: {}", e);
            return;
        }
    };

    let from = format!(
        "{} <{}>",
        effective_from_name(&snap.from_name, &state.site_name),
        snap.from_email
    );
    let subject = format!("New post: {}", post.title);
    let read_more = if snap.site_url.is_empty() {
        format!("/blog/{}", post.slug)
    } else {
        format!("{}/blog/{}", snap.site_url.trim_end_matches('/'), post.slug)
    };
    let body = format!(
        "{}\n\nRead more: {}\n\n--\nYou're receiving this because you subscribed.",
        post.content, read_more
    );

    let mut sent = 0usize;
    let mut failed = 0usize;
    for sub in &recipients {
        let to = match sub.email.parse() {
            Ok(v) => v,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let from_parsed = match from.parse() {
            Ok(v) => v,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let msg = match Message::builder()
            .from(from_parsed)
            .to(to)
            .subject(&subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.clone())
        {
            Ok(m) => m,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        match mailer.send(msg).await {
            Ok(_) => sent += 1,
            Err(_) => failed += 1,
        }
    }
    println!(
        "[newsletter] auto-send for post {} ('{}'): sent={} failed={}",
        post_id, post.title, sent, failed
    );
}

// ============================================================================
// Public handlers
// ============================================================================

#[derive(Clone)]
struct PublicState {
    db: Arc<CccDB>,
    /// Operator-configured site name rendered into public page chrome.
    site_name: Arc<String>,
}

#[derive(Template)]
#[template(path = "public_subscribe.html")]
struct PublicSubscribePage<'a> {
    site_name: String,
    success: Option<bool>,
    error: Option<&'a str>,
}

async fn public_subscribe_get(State(state): State<PublicState>) -> Response {
    render(PublicSubscribePage {
        site_name: state.site_name.as_ref().clone(),
        success: None,
        error: None,
    })
}

#[derive(Deserialize)]
struct SubscribeForm {
    email: String,
}

async fn public_subscribe_post(
    State(state): State<PublicState>,
    Form(form): Form<SubscribeForm>,
) -> Response {
    let email = form.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return render(PublicSubscribePage {
            site_name: state.site_name.as_ref().clone(),
            success: None,
            error: Some("That doesn't look like an email address."),
        });
    }
    if let Ok(Some(_)) = state.db.get_subscriber_by_email(&email) {
        // Already subscribed: silently treat as success to avoid email enumeration.
        return render(PublicSubscribePage {
            site_name: state.site_name.as_ref().clone(),
            success: Some(true),
            error: None,
        });
    }
    let now = unix_now();
    let sub = Subscriber {
        id: now,
        email,
        active: true,
        created_at: now,
    };
    if let Err(e) = state.db.upsert_subscriber(&sub) {
        return render(PublicSubscribePage {
            site_name: state.site_name.as_ref().clone(),
            success: None,
            error: Some(&format!("Could not subscribe: {}", e)),
        });
    }
    render(PublicSubscribePage {
        site_name: state.site_name.as_ref().clone(),
        success: Some(true),
        error: None,
    })
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Clone)]
struct AdminState {
    db: Arc<CccDB>,
    settings: Arc<SettingsStore>,
    /// Operator-configured site name; substituted for the untouched
    /// `from_name` default at send time (see `effective_from_name`).
    site_name: Arc<String>,
}

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage<'a> {
    subscribers: Vec<SubscriberRow>,
    active_count: usize,
    status_message: Option<&'a str>,
}

struct SubscriberRow {
    id: u64,
    email: String,
    active: bool,
    created_human: String,
}

async fn admin_list(State(state): State<AdminState>) -> Response {
    let raw = match state.db.all_subscribers() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let active_count = raw.iter().filter(|s| s.active).count();
    let rows: Vec<SubscriberRow> = raw
        .into_iter()
        .map(|s| SubscriberRow {
            id: s.id,
            email: s.email,
            active: s.active,
            created_human: format_unix_utc(s.created_at),
        })
        .collect();
    render(AdminListPage {
        subscribers: rows,
        active_count,
        status_message: None,
    })
}

async fn admin_delete(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_subscriber(id);
    Redirect::to("/admin/newsletter").into_response()
}

#[derive(Template)]
#[template(path = "admin_compose.html")]
struct AdminComposePage<'a> {
    recipient_count: usize,
    subject: String,
    body: String,
    error: Option<&'a str>,
}

async fn admin_compose_get(State(state): State<AdminState>) -> Response {
    let recipient_count = state
        .db
        .all_subscribers()
        .map(|v| v.into_iter().filter(|s| s.active).count())
        .unwrap_or(0);
    render(AdminComposePage {
        recipient_count,
        subject: String::new(),
        body: String::new(),
        error: None,
    })
}

#[derive(Deserialize)]
struct ComposeForm {
    subject: String,
    body: String,
}

async fn admin_send(State(state): State<AdminState>, Form(form): Form<ComposeForm>) -> Response {
    let snap = state.settings.snapshot();
    if snap.host.is_empty() || snap.from_email.is_empty() {
        return render_compose_error(
            &state,
            &form,
            "SMTP isn't configured yet. Add credentials in Settings first.",
        )
        .await;
    }
    let recipients: Vec<Subscriber> = match state.db.all_subscribers() {
        Ok(v) => v.into_iter().filter(|s| s.active).collect(),
        Err(e) => {
            return render_compose_error(
                &state,
                &form,
                &format!("Could not load subscribers: {}", e),
            )
            .await;
        }
    };
    if recipients.is_empty() {
        return render_compose_error(&state, &form, "No active subscribers to send to.").await;
    }

    let mailer = match build_mailer(&snap) {
        Ok(m) => m,
        Err(e) => {
            return render_compose_error(&state, &form, &format!("SMTP setup failed: {}", e)).await
        }
    };

    let from = format!(
        "{} <{}>",
        effective_from_name(&snap.from_name, &state.site_name),
        snap.from_email
    );
    let mut sent = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for sub in &recipients {
        let msg = Message::builder()
            .from(match from.parse() {
                Ok(v) => v,
                Err(_) => continue,
            })
            .to(match sub.email.parse() {
                Ok(v) => v,
                Err(_) => continue,
            })
            .subject(&form.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(form.body.clone());
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{}: {}", sub.email, e));
                continue;
            }
        };
        match mailer.send(msg).await {
            Ok(_) => sent += 1,
            Err(e) => errors.push(format!("{}: {}", sub.email, e)),
        }
    }

    if errors.is_empty() {
        let recipient_count = recipients.len();
        render(AdminListPage {
            subscribers: load_rows(&state.db),
            active_count: recipient_count,
            status_message: Some(&format!("Sent to {} subscriber(s).", sent)),
        })
    } else {
        let summary = format!(
            "Sent to {}. {} failed: {}",
            sent,
            errors.len(),
            errors.join("; ")
        );
        render_compose_error(&state, &form, &summary).await
    }
}

async fn render_compose_error(state: &AdminState, form: &ComposeForm, msg: &str) -> Response {
    let recipient_count = state
        .db
        .all_subscribers()
        .map(|v| v.into_iter().filter(|s| s.active).count())
        .unwrap_or(0);
    render(AdminComposePage {
        recipient_count,
        subject: form.subject.clone(),
        body: form.body.clone(),
        error: Some(msg),
    })
}

fn load_rows(db: &CccDB) -> Vec<SubscriberRow> {
    db.all_subscribers()
        .unwrap_or_default()
        .into_iter()
        .map(|s| SubscriberRow {
            id: s.id,
            email: s.email,
            active: s.active,
            created_human: format_unix_utc(s.created_at),
        })
        .collect()
}

/// Resolve the email sender name.
///
/// The sidecar's serde `Default` hard-codes `from_name = "Nuntius"` (the
/// platform name) because it cannot see `ccc.toml`. If the stored value
/// is still that untouched default, substitute the operator-configured
/// `[site] name` at send time. The override is purely in-memory — it is
/// never written back to `newsletter.toml` — so anything the operator
/// saves in the admin settings UI (i.e. any value other than the literal
/// default) always wins.
fn effective_from_name<'a>(stored: &'a str, site_name: &'a str) -> &'a str {
    if stored == "Nuntius" {
        site_name
    } else {
        stored
    }
}

fn build_mailer(s: &NewsletterSettings) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&s.host)
        .map_err(|e| e.to_string())?
        .port(s.port);
    if !s.username.is_empty() {
        builder = builder.credentials(Credentials::new(s.username.clone(), s.password.clone()));
    }
    Ok(builder.build())
}

// ----- Settings -----

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage {
    host: String,
    port: u16,
    username: String,
    password: String,
    from_email: String,
    from_name: String,
    auto_send_blog_posts: bool,
    site_url: String,
    saved: Option<bool>,
}

async fn admin_settings_get(State(state): State<AdminState>) -> Response {
    let s = state.settings.snapshot();
    render(AdminSettingsPage {
        host: s.host,
        port: s.port,
        username: s.username,
        password: s.password,
        from_email: s.from_email,
        from_name: s.from_name,
        auto_send_blog_posts: s.auto_send_blog_posts,
        site_url: s.site_url,
        saved: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    host: String,
    port: u16,
    username: String,
    password: String,
    from_email: String,
    from_name: String,
    #[serde(default)]
    auto_send_blog_posts: Option<String>,
    #[serde(default)]
    site_url: String,
}

async fn admin_settings_post(
    State(state): State<AdminState>,
    Form(form): Form<SettingsForm>,
) -> Response {
    let new = NewsletterSettings {
        host: form.host.trim().to_string(),
        port: form.port,
        username: form.username.trim().to_string(),
        password: form.password,
        from_email: form.from_email.trim().to_string(),
        from_name: form.from_name.trim().to_string(),
        auto_send_blog_posts: form.auto_send_blog_posts.is_some(),
        site_url: form.site_url.trim().to_string(),
    };
    let _ = state.settings.replace(new);
    Redirect::to("/admin/newsletter/settings?saved=1").into_response()
}

// ============================================================================
// Helpers
// ============================================================================
// render / unix_now / format_unix_utc (the `YYYY-MM-DD HH:MM UTC` shape
// the subscriber list has always shown) all live in ccc-plugin-utils.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_from_name_default_yields_the_site_name() {
        // Stored value is the untouched serde default -> site name wins.
        assert_eq!(
            effective_from_name("Nuntius", "Radio Free Harbor"),
            "Radio Free Harbor"
        );
        // Operator customized the sender name in the admin UI -> it wins.
        assert_eq!(
            effective_from_name("The Harbor Desk", "Radio Free Harbor"),
            "The Harbor Desk"
        );
        // Default deployment (site name left as "Nuntius") is a no-op.
        assert_eq!(effective_from_name("Nuntius", "Nuntius"), "Nuntius");
    }

    #[test]
    fn subscribe_page_renders_the_configured_site_name() {
        let html = PublicSubscribePage {
            site_name: "Radio Free Harbor".into(),
            success: None,
            error: None,
        }
        .render()
        .unwrap();
        assert!(
            html.contains("<title>Subscribe — Radio Free Harbor</title>"),
            "title should carry the site name: {html}"
        );
        assert!(
            html.contains("Subscribe to the Radio Free Harbor newsletter."),
            "meta description should carry the site name: {html}"
        );
        assert!(
            html.contains(">Radio Free Harbor</a>"),
            "brand link should carry the site name: {html}"
        );
        // Platform attribution is deliberate and must stay.
        assert!(html.contains("Powered by Nuntius"));
    }
}
