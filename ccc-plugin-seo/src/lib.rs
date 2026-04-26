//! SEO plugin: sitemap.xml, robots.txt, configurable site metadata.
//!
//! Public:
//! - `GET /sitemap.xml` — XML sitemap of all published blog posts (more
//!   sources land as their plugins mature).
//! - `GET /robots.txt` — verbatim from settings.
//!
//! Admin:
//! - `GET/POST /admin/seo` — site name, public URL, default description,
//!   robots.txt body.

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use ccc_database::{CccDB, PostStatus};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeoSettings {
    pub site_name: String,
    pub site_url: String,
    pub default_description: String,
    pub robots_txt: String,
}

impl Default for SeoSettings {
    fn default() -> Self {
        Self {
            site_name: "Creator Command Center".into(),
            site_url: "https://example.com".into(),
            default_description: "A self-hosted creator station.".into(),
            robots_txt: "User-agent: *\nAllow: /\n".into(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    seo: SeoSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<SeoSettings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let s = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| PluginError::InitError(format!("seo IO: {}", e)))?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.seo
        } else {
            SeoSettings::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(s),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> SeoSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: SeoSettings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { seo: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PluginError::InitError(format!("seo IO: {}", e)))?;
            }
        }
        std::fs::write(&self.path, raw)
            .map_err(|e| PluginError::InitError(format!("seo IO: {}", e)))?;
        Ok(())
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginSeoPlugin {
    settings: Arc<SettingsStore>,
}

impl CccPluginSeoPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("seo")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("seo.toml"));
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        Ok(Self { settings: store })
    }
}

#[derive(Clone)]
struct PublicState {
    db: Arc<CccDB>,
    settings: Arc<SettingsStore>,
}

#[derive(Clone)]
struct AdminState {
    settings: Arc<SettingsStore>,
}

impl Plugin for CccPluginSeoPlugin {
    fn name(&self) -> &'static str {
        "SEO"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let s = self.settings.snapshot();
        println!("Initializing SEO: site={}", s.site_url);
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = PublicState {
            db: ctx.db.clone(),
            settings: self.settings.clone(),
        };
        Some(
            Router::new()
                .route("/sitemap.xml", get(sitemap))
                .route("/robots.txt", get(robots))
                .with_state(state),
        )
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/seo", get(admin_get).post(admin_post))
                .with_state(AdminState {
                    settings: self.settings.clone(),
                }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "SEO",
            href: "/admin/seo".into(),
            icon: "🔍",
            sort: 600,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Public handlers
// ============================================================================

async fn sitemap(State(state): State<PublicState>) -> Response {
    let s = state.settings.snapshot();
    let base = s.site_url.trim_end_matches('/').to_string();
    let posts = state
        .db
        .latest_published_blog_posts(10_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.status == PostStatus::Published)
        .collect::<Vec<_>>();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    // The site root + the blog index.
    xml.push_str(&format!("  <url><loc>{}/</loc></url>\n", base));
    xml.push_str(&format!("  <url><loc>{}/blog/</loc></url>\n", base));
    for p in posts {
        let lastmod = iso_date(p.updated_at);
        xml.push_str(&format!(
            "  <url><loc>{}/blog/{}</loc><lastmod>{}</lastmod></url>\n",
            base,
            xml_escape(&p.slug),
            lastmod
        ));
    }
    xml.push_str("</urlset>\n");

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/xml; charset=utf-8",
        )],
        xml,
    )
        .into_response()
}

async fn robots(State(state): State<PublicState>) -> Response {
    let s = state.settings.snapshot();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        s.robots_txt,
    )
        .into_response()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn iso_date(secs: u64) -> String {
    let (y, mo, d, _, _) = unix_to_ymdhm(secs);
    format!("{:04}-{:02}-{:02}", y, mo, d)
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
struct AdminSettingsPage {
    site_name: String,
    site_url: String,
    default_description: String,
    robots_txt: String,
    saved: Option<bool>,
}

async fn admin_get(State(state): State<AdminState>) -> Response {
    let s = state.settings.snapshot();
    render(AdminSettingsPage {
        site_name: s.site_name,
        site_url: s.site_url,
        default_description: s.default_description,
        robots_txt: s.robots_txt,
        saved: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    site_name: String,
    site_url: String,
    default_description: String,
    robots_txt: String,
}

async fn admin_post(State(state): State<AdminState>, Form(form): Form<SettingsForm>) -> Response {
    let new = SeoSettings {
        site_name: form.site_name.trim().to_string(),
        site_url: form.site_url.trim().to_string(),
        default_description: form.default_description,
        robots_txt: form.robots_txt,
    };
    let _ = state.settings.replace(new);
    Redirect::to("/admin/seo?saved=1").into_response()
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
