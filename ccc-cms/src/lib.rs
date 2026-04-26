//! Content Management System for the Creator Command Center (CCC).
//!
//! This crate is itself a plugin. When enabled in `ccc.toml`, it:
//!
//! 1. Mounts the `/admin` URL prefix.
//! 2. On first run (no admin account on disk), redirects everything to a
//!    one-time setup wizard that creates the admin user.
//! 3. After setup, gates `/admin/*` behind a session-cookie login.
//! 4. Renders a three-column shell (left nav / main / right rail) and
//!    merges every other enabled plugin's `admin_routes()` underneath it.
//!
//! When the CMS plugin is *disabled*, no admin surface exists — admin
//! routes from other plugins do not mount. See the `Plugin::admin_routes`
//! security invariant in `ccc-core`.

use ccc_core::{Plugin, PluginError};
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

pub mod auth;
pub mod middleware;
pub mod routes;
pub mod templates;

pub use auth::{AdminAccount, AdminStore, AuthError};
pub use routes::{build_admin_tree, DashboardCtx};

/// The CMS plugin.
///
/// Holds the path to the sidecar `admin.toml` that stores the (argon2-hashed)
/// admin credentials. Constructed via `from_config` from the `[plugin.cms]`
/// subtable.
pub struct CmsPlugin {
    /// Path to the sidecar file holding the admin account (argon2 hash).
    /// Defaults to `admin.toml` next to `ccc.toml`.
    pub admin_store_path: PathBuf,

    /// Cookie-signing key. If absent at startup, one is generated and
    /// persisted to `admin.toml`.
    pub cookie_key: Vec<u8>,

    /// Lazy-loaded admin store, populated at `init()` time.
    pub store: Arc<AdminStore>,
}

impl CmsPlugin {
    /// Build a CMS plugin from the loaded `CccConfig`. Reads the optional
    /// `[plugin.cms]` subtable for `admin_store_path`. Fails only on I/O
    /// errors against the sidecar file.
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let admin_store_path = cfg
            .plugin
            .get("cms")
            .and_then(|t| t.get("admin_store_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("admin.toml"));

        let store = AdminStore::load_or_init(&admin_store_path).map_err(|e| {
            PluginError::InitError(format!("CMS: failed to load admin store: {}", e))
        })?;

        let cookie_key = store.cookie_key.clone();

        Ok(Self {
            admin_store_path,
            cookie_key,
            store: Arc::new(store),
        })
    }
}

impl Plugin for CmsPlugin {
    fn name(&self) -> &'static str {
        "Content Management System"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let setup_state = if self.store.is_initialized() {
            "ready"
        } else {
            "first-run setup pending"
        };
        println!(
            "Initializing CMS (admin store: {}, state: {})",
            self.admin_store_path.display(),
            setup_state
        );
        Ok(())
    }

    // Note: no `routes()` here. The `/admin` tree is built by the host
    // via `ccc_cms::routes::build_admin_tree(store, plugin_admin)` so it
    // can include every other enabled plugin's `admin_routes()`. Doing
    // it inside this `routes()` impl would mean the CMS plugin couldn't
    // see what its sibling plugins contributed.

    fn as_any(&self) -> &dyn Any {
        self
    }
}

