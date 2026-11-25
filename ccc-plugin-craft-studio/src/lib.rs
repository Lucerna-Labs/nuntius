//! ArtCraft Studio plugins for Nuntius: embeds `storytold/wordcraft`
//! (`wordcraft-web`) and `storytold/pdfcraft` (`pdfcraft-web`) directly
//! inside the Nuntius Creator Command Center `/admin` shell.

use askama::Template;
use axum::{
    extract::State,
    response::Response,
    routing::get,
    Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, CccConfig, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_plugin_utils::render;
use serde::Deserialize;
use std::any::Any;
use std::path::{Path, PathBuf};
use tower_http::services::ServeDir;

#[derive(Debug, Clone, Deserialize, Default)]
struct CraftPluginSettings {
    bundle_dir: Option<String>,
}

fn resolve_bundle_dir(cfg: &CccConfig, plugin_id: &str, default_subdir: &str) -> PathBuf {
    let settings: CraftPluginSettings = cfg
        .plugin
        .get(plugin_id)
        .and_then(|v| v.clone().try_into().ok())
        .unwrap_or_default();
    if let Some(dir) = settings.bundle_dir {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir.trim());
        }
    }
    let cwd_candidate = PathBuf::from("web-apps").join(default_subdir);
    if cwd_candidate.join("index.html").is_file() {
        return cwd_candidate;
    }
    // Fallback to workspace-relative web-apps/<subdir> for dev runs.
    let manifest_candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("web-apps")
        .join(default_subdir);
    if manifest_candidate.join("index.html").is_file() {
        return manifest_candidate;
    }
    cwd_candidate
}

#[derive(Clone)]
struct StudioState {
    app_id: &'static str,
    app_name: &'static str,
    tagline: &'static str,
    app_mount_url: &'static str,
    bundle_dir: PathBuf,
}

#[derive(Template)]
#[template(path = "studio.html")]
struct StudioPage {
    app_id: &'static str,
    app_name: &'static str,
    tagline: &'static str,
    app_mount_url: &'static str,
    bundle_ready: bool,
    bundle_dir_display: String,
}

async fn studio_page(State(state): State<StudioState>) -> Response {
    let bundle_ready = state.bundle_dir.join("index.html").is_file();
    render(StudioPage {
        app_id: state.app_id,
        app_name: state.app_name,
        tagline: state.tagline,
        app_mount_url: state.app_mount_url,
        bundle_ready,
        bundle_dir_display: state.bundle_dir.display().to_string(),
    })
}

fn build_studio_router(state: StudioState, base_path: &'static str) -> Router {
    let serve = ServeDir::new(state.bundle_dir.clone());
    Router::new()
        .route(base_path, get(studio_page))
        .nest_service(&format!("{base_path}/app"), serve)
        .with_state(state)
}

// ============================================================================
// WordCraft Plugin
// ============================================================================

pub struct WordCraftPlugin {
    bundle_dir: PathBuf,
}

impl WordCraftPlugin {
    pub fn from_config(cfg: &CccConfig) -> Self {
        Self {
            bundle_dir: resolve_bundle_dir(cfg, "wordcraft", "wordcraft"),
        }
    }
}

impl Plugin for WordCraftPlugin {
    fn name(&self) -> &'static str {
        "WordCraft"
    }
    fn version(&self) -> &'static str {
        "0.19.1"
    }
    fn id(&self) -> String {
        "wordcraft".into()
    }
    fn kind(&self) -> &'static str {
        "STUDIO"
    }
    fn icon(&self) -> &'static str {
        "icon-blog"
    }
    fn admin_url(&self) -> String {
        "/admin/wordcraft".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let ready = self.bundle_dir.join("index.html").is_file();
        PluginStatus {
            state: if ready {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": ready,
                "bundle_dir": self.bundle_dir.display().to_string(),
                "app": "storytold/wordcraft (WASM)",
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!(
            "Initializing WordCraft Studio plugin (bundle: {}).",
            self.bundle_dir.display()
        );
        Ok(())
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(build_studio_router(
            StudioState {
                app_id: "wordcraft",
                app_name: "WordCraft",
                tagline: "ArtCraft pure-Rust word processor (DOCX / Markdown / PDF export) running in-browser.",
                app_mount_url: "/admin/wordcraft/app/",
                bundle_dir: self.bundle_dir.clone(),
            },
            "/wordcraft",
        ))
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "WordCraft",
            href: "/admin/wordcraft".into(),
            icon: "📄",
            sort: 210,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// PdfCraft Plugin
// ============================================================================

pub struct PdfCraftPlugin {
    bundle_dir: PathBuf,
}

impl PdfCraftPlugin {
    pub fn from_config(cfg: &CccConfig) -> Self {
        Self {
            bundle_dir: resolve_bundle_dir(cfg, "pdfcraft", "pdfcraft"),
        }
    }
}

impl Plugin for PdfCraftPlugin {
    fn name(&self) -> &'static str {
        "PdfCraft"
    }
    fn version(&self) -> &'static str {
        "0.22.0"
    }
    fn id(&self) -> String {
        "pdfcraft".into()
    }
    fn kind(&self) -> &'static str {
        "STUDIO"
    }
    fn icon(&self) -> &'static str {
        "icon-blog"
    }
    fn admin_url(&self) -> String {
        "/admin/pdfcraft".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let ready = self.bundle_dir.join("index.html").is_file();
        PluginStatus {
            state: if ready {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": ready,
                "bundle_dir": self.bundle_dir.display().to_string(),
                "app": "storytold/pdfcraft (WASM)",
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!(
            "Initializing PdfCraft Studio plugin (bundle: {}).",
            self.bundle_dir.display()
        );
        Ok(())
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(build_studio_router(
            StudioState {
                app_id: "pdfcraft",
                app_name: "PdfCraft",
                tagline: "ArtCraft pure-Rust PDF reader, editor & OCR studio running in-browser.",
                app_mount_url: "/admin/pdfcraft/app/",
                bundle_dir: self.bundle_dir.clone(),
            },
            "/pdfcraft",
        ))
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "PdfCraft",
            href: "/admin/pdfcraft".into(),
            icon: "📑",
            sort: 220,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_template_renders_ready_and_missing_states() {
        let ready_html = StudioPage {
            app_id: "wordcraft",
            app_name: "WordCraft",
            tagline: "Word processor",
            app_mount_url: "/admin/wordcraft/app/",
            bundle_ready: true,
            bundle_dir_display: "web-apps/wordcraft".into(),
        }
        .render()
        .unwrap();
        assert!(ready_html.contains("<iframe"));
        assert!(ready_html.contains("/admin/wordcraft/app/"));

        let missing_html = StudioPage {
            app_id: "pdfcraft",
            app_name: "PdfCraft",
            tagline: "PDF editor",
            app_mount_url: "/admin/pdfcraft/app/",
            bundle_ready: false,
            bundle_dir_display: "web-apps/pdfcraft".into(),
        }
        .render()
        .unwrap();
        assert!(!missing_html.contains("<iframe"));
        assert!(missing_html.contains("web bundle not installed yet"));
    }
}
