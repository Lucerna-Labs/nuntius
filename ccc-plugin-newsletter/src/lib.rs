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
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError};
use ccc_database::{CccDB, PostStatus, Subscriber};
use lettre::message::{header::ContentType, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

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
            from_name: "Creator Command Center".into(),
            auto_send_blog_posts: false,
            site_url: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    newsletter: NewsletterSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<NewsletterSettings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let nl = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| PluginError::InitError(format!("newsletter IO: {}", e)))?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.newsletter
        } else {
            NewsletterSettings::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(nl),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> NewsletterSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: NewsletterSettings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { newsletter: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PluginError::InitError(format!("newsletter IO: {}", e)))?;
            }
        }
        std::fs::write(&self.path, raw)
            .map_err(|e| PluginError::InitError(format!("newsletter IO: {}", e)))?;
        Ok(())
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginNewsletterPlugin {
    settings: Arc<SettingsStore>,
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
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        Ok(Self { settings: store })
    }
}

impl Plugin for CccPluginNewsletterPlugin {
    fn name(&self) -> &'static str {
        "Newsletter"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
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
                .with_state(PublicState { db: ctx.db.clone() }),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let admin_state = AdminState {
            db: ctx.db.clone(),
            settings: self.settings.clone(),
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

    let from = format!("{} <{}>", snap.from_name, snap.from_email);
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
}

#[derive(Template)]
#[template(path = "public_subscribe.html")]
struct PublicSubscribePage<'a> {
    success: Option<bool>,
    error: Option<&'a str>,
}

async fn public_subscribe_get() -> Response {
    render(PublicSubscribePage {
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
            success: None,
            error: Some("That doesn't look like an email address."),
        });
    }
    if let Ok(Some(_)) = state.db.get_subscriber_by_email(&email) {
        // Already subscribed: silently treat as success to avoid email enumeration.
        return render(PublicSubscribePage {
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
            success: None,
            error: Some(&format!("Could not subscribe: {}", e)),
        });
    }
    render(PublicSubscribePage {
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
            created_human: format_unix(s.created_at),
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

    let from = format!("{} <{}>", snap.from_name, snap.from_email);
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
            created_human: format_unix(s.created_at),
        })
        .collect()
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

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_unix(ts: u64) -> String {
    let (y, mo, d, h, mi) = unix_to_ymdhm(ts);
    format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", y, mo, d, h, mi)
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
