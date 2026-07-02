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
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_database::{CccDB, PostStatus};
use ccc_plugin_utils::{render, unix_to_ymdhm, xml_escape};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

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
            site_name: "Nuntius".into(),
            site_url: "https://example.com".into(),
            default_description: "A self-hosted creator station.".into(),
            robots_txt: "User-agent: *\nAllow: /\n".into(),
        }
    }
}

/// Type alias preserving the historical name. The actual storage is
/// `TomlSidecar<SeoSettings>` from `ccc-plugin-utils`; this alias lets
/// the rest of the file keep using `SettingsStore` unchanged.
pub type SettingsStore = ccc_plugin_utils::TomlSidecar<SeoSettings>;

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
        let store = Arc::new(
            SettingsStore::load_or_init(&path)
                .map_err(|e| PluginError::InitError(e.to_string()))?,
        );
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

    fn id(&self) -> String {
        "seo".into()
    }
    fn kind(&self) -> &'static str {
        "METADATA"
    }
    fn icon(&self) -> &'static str {
        "icon-seo"
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let s = self.settings.snapshot();
        let placeholder = s.site_url.contains("example.com");
        PluginStatus {
            state: if placeholder {
                PluginLifecycle::Unconfigured
            } else {
                PluginLifecycle::Configured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": !placeholder,
                "site_name": s.site_name,
                "site_url": s.site_url,
                "default_description": s.default_description,
                "warning": if placeholder { Some("site_url still pointing at example.com".to_string()) } else { None },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
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

// xml_escape and the date math live in ccc-plugin-utils; only the
// sitemap-specific `<lastmod>` shape stays here.
fn iso_date(secs: u64) -> String {
    let (y, mo, d, _, _) = unix_to_ymdhm(secs);
    format!("{:04}-{:02}-{:02}", y, mo, d)
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

