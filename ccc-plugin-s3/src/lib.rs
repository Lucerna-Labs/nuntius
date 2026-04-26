//! S3-compatible object storage plugin.
//!
//! Provides shared object storage for any other plugin that wants to upload
//! media (podcast audio, video segments, blog images). The settings page
//! in admin lets the operator point CCC at AWS S3, Cloudflare R2,
//! Backblaze B2, MinIO, or any other S3-compatible service.
//!
//! Public API for sibling plugins (resolved via
//! `manager.find::<CccPluginS3Plugin>()`):
//! - `put_object(key, bytes, content_type) -> Result`
//! - `delete_object(key) -> Result`
//! - `public_url_for(key) -> Option<String>`
//! - `test_connection() -> Result<(), String>`

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use s3::creds::Credentials;
use s3::{Bucket, Region};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct S3Settings {
    pub bucket: String,
    pub region: String,
    /// Optional explicit endpoint URL — required for non-AWS providers.
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    /// Where uploaded files are publicly reachable (built into URLs we
    /// hand to the audience).
    pub public_url_base: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    s3: S3Settings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<S3Settings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let s3 = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| PluginError::InitError(format!("s3 IO: {}", e)))?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.s3
        } else {
            S3Settings::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(s3),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> S3Settings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: S3Settings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { s3: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PluginError::InitError(format!("s3 IO: {}", e)))?;
            }
        }
        std::fs::write(&self.path, raw)
            .map_err(|e| PluginError::InitError(format!("s3 IO: {}", e)))?;
        Ok(())
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginS3Plugin {
    settings: Arc<SettingsStore>,
}

impl CccPluginS3Plugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("s3")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("s3.toml"));
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        Ok(Self { settings: store })
    }

    /// Returns true iff every required field is filled in.
    pub fn is_configured(&self) -> bool {
        let s = self.settings.snapshot();
        !s.bucket.is_empty()
            && !s.region.is_empty()
            && !s.access_key.is_empty()
            && !s.secret_key.is_empty()
    }

    fn open_bucket(&self) -> Result<Bucket, String> {
        let s = self.settings.snapshot();
        if !self.is_configured() {
            return Err("S3 not configured".into());
        }
        let region = if s.endpoint.is_empty() {
            Region::Custom {
                region: s.region.clone(),
                endpoint: format!("https://s3.{}.amazonaws.com", s.region),
            }
        } else {
            Region::Custom {
                region: s.region.clone(),
                endpoint: s.endpoint.clone(),
            }
        };
        let creds = Credentials::new(Some(&s.access_key), Some(&s.secret_key), None, None, None)
            .map_err(|e| e.to_string())?;
        let bucket = Bucket::new(&s.bucket, region, creds).map_err(|e| e.to_string())?;
        Ok(bucket)
    }

    /// Upload bytes to a key. Returns the object's public URL on success.
    pub async fn put_object(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<String, String> {
        let bucket = self.open_bucket()?;
        bucket
            .put_object_with_content_type(key, bytes, content_type)
            .await
            .map_err(|e| e.to_string())?;
        Ok(self.public_url_for(key))
    }

    pub async fn delete_object(&self, key: &str) -> Result<(), String> {
        let bucket = self.open_bucket()?;
        bucket.delete_object(key).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn public_url_for(&self, key: &str) -> String {
        let s = self.settings.snapshot();
        let base = if s.public_url_base.is_empty() {
            // Fall back to AWS-style virtual-host URL.
            format!("https://{}.s3.{}.amazonaws.com", s.bucket, s.region)
        } else {
            s.public_url_base.trim_end_matches('/').to_string()
        };
        format!("{}/{}", base, key.trim_start_matches('/'))
    }

    /// Probe the bucket via HEAD on the bucket root.
    pub async fn test_connection(&self) -> Result<String, String> {
        let bucket = self.open_bucket()?;
        // list_objects with a tiny page is the most portable "can I reach
        // and authenticate to this bucket?" test.
        bucket
            .list("".into(), Some("/".into()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("Connected to bucket '{}'.", bucket.name()))
    }
}

impl Plugin for CccPluginS3Plugin {
    fn name(&self) -> &'static str {
        "Object Storage (S3)"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let s = self.settings.snapshot();
        let configured = self.is_configured();
        println!(
            "Initializing S3: bucket='{}' configured={}",
            s.bucket, configured
        );
        Ok(())
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/storage", get(admin_get).post(admin_post))
                .with_state(AdminState {
                    plugin: Arc::new(PluginHandle {
                        settings: self.settings.clone(),
                    }),
                }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Storage",
            href: "/admin/storage".into(),
            icon: "📦",
            sort: 400,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Admin handlers
// ============================================================================

/// Lightweight wrapper that owns the settings handle for admin handlers.
/// Avoids cycling references through the manager.
struct PluginHandle {
    settings: Arc<SettingsStore>,
}

impl PluginHandle {
    fn snapshot(&self) -> S3Settings {
        self.settings.snapshot()
    }
    fn replace(&self, new: S3Settings) -> Result<(), PluginError> {
        self.settings.replace(new)
    }
    async fn test_connection(&self) -> Result<String, String> {
        // Build a temporary plugin with the same settings to reuse logic.
        let p = CccPluginS3Plugin {
            settings: self.settings.clone(),
        };
        p.test_connection().await
    }
}

#[derive(Clone)]
struct AdminState {
    plugin: Arc<PluginHandle>,
}

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage<'a> {
    bucket: String,
    region: String,
    endpoint: String,
    access_key: String,
    secret_key: String,
    public_url_base: String,
    status: Option<String>,
    error: Option<&'a str>,
}

async fn admin_get(State(state): State<AdminState>) -> Response {
    let s = state.plugin.snapshot();
    render(AdminSettingsPage {
        bucket: s.bucket,
        region: s.region,
        endpoint: s.endpoint,
        access_key: s.access_key,
        secret_key: s.secret_key,
        public_url_base: s.public_url_base,
        status: None,
        error: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    bucket: String,
    region: String,
    endpoint: String,
    access_key: String,
    secret_key: String,
    public_url_base: String,
    action: String,
}

async fn admin_post(State(state): State<AdminState>, Form(form): Form<SettingsForm>) -> Response {
    let new = S3Settings {
        bucket: form.bucket.trim().to_string(),
        region: form.region.trim().to_string(),
        endpoint: form.endpoint.trim().to_string(),
        access_key: form.access_key.trim().to_string(),
        secret_key: form.secret_key.trim().to_string(),
        public_url_base: form.public_url_base.trim().to_string(),
    };
    if let Err(e) = state.plugin.replace(new.clone()) {
        return render(AdminSettingsPage {
            bucket: new.bucket,
            region: new.region,
            endpoint: new.endpoint,
            access_key: new.access_key,
            secret_key: new.secret_key,
            public_url_base: new.public_url_base,
            status: None,
            error: Some(&format!("Could not save: {}", e)),
        });
    }

    if form.action != "test" {
        return Redirect::to("/admin/storage").into_response();
    }

    // Save+test path: run the connection probe and re-render.
    match state.plugin.test_connection().await {
        Ok(msg) => render(AdminSettingsPage {
            bucket: new.bucket,
            region: new.region,
            endpoint: new.endpoint,
            access_key: new.access_key,
            secret_key: new.secret_key,
            public_url_base: new.public_url_base,
            status: Some(msg),
            error: None,
        }),
        Err(e) => render(AdminSettingsPage {
            bucket: new.bucket,
            region: new.region,
            endpoint: new.endpoint,
            access_key: new.access_key,
            secret_key: new.secret_key,
            public_url_base: new.public_url_base,
            status: None,
            error: Some(&format!("Connection failed: {}", e)),
        }),
    }
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
