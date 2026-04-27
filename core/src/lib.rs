//! Core functionality for the Creator Command Center (CCC) Rust rewrite.
//!
//! This crate contains the essential building blocks of the application,
//! including the plugin architecture, fundamental data structures, and traits.

pub mod activitypub;
pub mod config;
pub mod services;

pub use config::{CccConfig, ConfigError, DatabaseConfig, PluginsConfig, ServerConfig};
pub use services::{MembershipService, ServiceRegistry};

use axum::Router;
use ccc_database::CccDB;
use std::any::Any;
use std::sync::Arc;

/// Cross-plugin events. Adding variants is non-breaking — subscribers
/// pattern-match and ignore unknown variants.
#[derive(Clone, Debug)]
pub enum Event {
    /// A blog post just transitioned to the Published state.
    BlogPostPublished { id: u64 },
    /// A podcast episode just transitioned to the Published state.
    PodcastEpisodePublished { id: u64 },
    /// A video podcast episode just transitioned to the Published state.
    VideoPodcastPublished { id: u64 },
}

/// In-process event bus. Wraps a `tokio::sync::broadcast::Sender` so any
/// plugin can publish; any plugin that calls `subscribe()` receives every
/// future event. The CLI builds one at startup and shares it via
/// `AppContext::events`.
pub struct EventBus {
    sender: tokio::sync::broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(256);
        Self { sender }
    }

    /// Best-effort publish. If there are no subscribers the send simply
    /// returns Err; we ignore it on purpose so plugins don't have to
    /// know whether anyone is listening.
    pub fn publish(&self, event: Event) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared runtime context handed to every enabled plugin.
///
/// This is the single dependency injection seam between the host and plugins.
/// Add new fields here as the platform grows (config, http client, secrets).
/// Plugins ignore fields they don't use.
#[derive(Clone)]
pub struct AppContext {
    pub db: Arc<CccDB>,
    pub events: Arc<EventBus>,
    /// Cross-plugin service registry. Plugins publish capabilities they
    /// expose (e.g. `Arc<dyn MembershipService>`); consumers look them
    /// up by type. When the providing plugin is disabled, lookups
    /// return `None` and consumers should degrade gracefully.
    pub services: Arc<ServiceRegistry>,
}

/// One row in the admin shell's left rail. Serialized at
/// `/admin/api/plugins`. The shell groups `primary == true` under
/// "Broadcast" and the rest under "Plugins".
#[derive(Clone, Debug, serde::Serialize)]
pub struct RailRow {
    pub id: String,
    pub name: &'static str,
    pub kind: &'static str,
    pub icon: &'static str,
    pub primary: bool,
    pub state: PluginLifecycle,
    pub badge: Option<String>,
}

/// A sidebar entry a plugin contributes to the CMS admin shell.
///
/// `icon` is rendered as-is into the template; for now any string the
/// shell template accepts works (emoji, single character, future-proof
/// for SVG sprites). Keep labels short — they live in the left rail.
#[derive(Clone, Debug)]
pub struct AdminNavEntry {
    pub label: &'static str,
    pub href: String,
    pub icon: &'static str,
    /// Lower numbers sort first. Pick a stable value per plugin so the
    /// nav order stays predictable across rebuilds.
    pub sort: i32,
}

/// Per-plugin lifecycle status surfaced to the admin shell.
///
/// Named `PluginLifecycle` (not `PluginState`) to avoid colliding with
/// the `PluginState` axum router-state structs that several plugins
/// already define internally.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginLifecycle {
    /// Plugin is configured and idle/operational.
    Configured,
    /// Plugin's required configuration is missing — it loaded with defaults
    /// but won't do meaningful work until configured.
    Unconfigured,
    /// Plugin is actively producing output (radio is streaming, etc.).
    Live,
}

/// What `Plugin::status` returns to the admin shell.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PluginStatus {
    pub state: PluginLifecycle,
    /// Optional unread/backlog count rendered in the rail badge.
    pub badge: Option<String>,
    /// Plugin-specific JSON detail rendered in the slide-out panel. The
    /// Nuntius shell knows how to render each plugin's payload shape.
    pub detail: serde_json::Value,
}

impl Default for PluginStatus {
    fn default() -> Self {
        Self {
            state: PluginLifecycle::Configured,
            badge: None,
            detail: serde_json::json!({}),
        }
    }
}

/// A trait that all plugins must implement to hook into the Creator Command Center (CCC) core.
///
/// **Lifecycle contract:** if a plugin is *disabled* in `ccc.toml`, it is
/// never constructed — `new()` is not called, no `Plugin` value exists, no
/// memory is allocated, no router is registered, no background task spawned.
/// "Disabled" means *absent*, not "idle."
pub trait Plugin: Send + Sync {
    /// Returns the human-readable name of the plugin (e.g. "Radio Station").
    fn name(&self) -> &'static str;

    /// Returns the version of the plugin.
    fn version(&self) -> &'static str;

    /// URL slug for `/admin/api/plugins/{id}` routing and the admin rail.
    /// Default: a kebab-cased lowercase form of `name()`.
    fn id(&self) -> String {
        let n = self.name();
        let mut out = String::with_capacity(n.len());
        let mut prev_dash = true;
        for c in n.chars() {
            if c.is_ascii_alphanumeric() {
                out.extend(c.to_lowercase());
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        out
    }

    /// Short tag rendered next to the plugin name in admin surfaces
    /// (e.g. "BROADCAST", "PUBLISHING", "AUDIO"). Default: "PLUGIN".
    fn kind(&self) -> &'static str {
        "PLUGIN"
    }

    /// Icon key from the admin-shell icon set ("icon-radio", "icon-blog",
    /// etc.). Plugins that don't set one fall back to a monogram.
    fn icon(&self) -> &'static str {
        "icon-default"
    }

    /// True iff this plugin owns the admin shell's "home" stage — only
    /// the radio engine should return true.
    fn primary(&self) -> bool {
        false
    }

    /// Initialize the plugin. Called once when the plugin is registered.
    ///
    /// Set up routes, initialize state, or register extensions here.
    fn init(&mut self) -> Result<(), PluginError>;

    /// HTTP routes contributed by this plugin.
    ///
    /// Default: no routes. Plugins that expose endpoints override this and
    /// build a `Router` using whatever they need from `ctx`.
    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        None
    }

    /// Admin-only HTTP routes contributed by this plugin.
    ///
    /// These are *only* mounted if the `ccc-cms` plugin is enabled — that
    /// plugin owns the admin URL prefix, the auth middleware, and the
    /// merging of all `admin_routes()` returned here.
    ///
    /// **Security invariant:** if CMS is disabled, admin routes do not
    /// mount, period. They never go on the wire unprotected.
    ///
    /// Default: no admin routes.
    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        None
    }

    /// Sidebar entries this plugin contributes to the CMS admin shell.
    ///
    /// Returned per-plugin and merged by `PluginManager::merged_admin_nav`.
    /// The CMS dashboard renders them in the left nav, sorted by
    /// `AdminNavEntry::sort`.
    ///
    /// Default: no entries.
    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        Vec::new()
    }

    /// Live status payload for the admin rail + slide-out panel. The rail
    /// renders `state` as a colored dot and `badge` as a chip; the panel
    /// renders `detail` (plugin-specific JSON). Default: configured, no
    /// detail. Override to surface real state.
    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        PluginStatus::default()
    }

    /// Return an `Any` reference to allow downcasting if necessary for specific plugin interactions.
    fn as_any(&self) -> &dyn Any;
}

/// An error that can occur during plugin initialization or execution.
#[derive(Debug)]
pub enum PluginError {
    /// An initialization error with a description.
    InitError(String),
    /// An execution error with a description.
    ExecutionError(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::InitError(msg) => write!(f, "Plugin initialization error: {}", msg),
            PluginError::ExecutionError(msg) => write!(f, "Plugin execution error: {}", msg),
        }
    }
}

impl std::error::Error for PluginError {}

/// A manager responsible for loading, initializing, and holding references to plugins.
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    /// Create a new, empty `PluginManager`.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a new plugin. Calls `init()` and stores it.
    pub fn register<P: Plugin + 'static>(&mut self, mut plugin: P) -> Result<(), PluginError> {
        plugin.init()?;
        self.plugins.push(Box::new(plugin));
        Ok(())
    }

    /// Iterate over registered plugins.
    pub fn plugins(&self) -> impl Iterator<Item = &Box<dyn Plugin>> {
        self.plugins.iter()
    }

    /// Build a single `axum::Router` by merging routes from every plugin
    /// that contributed any. Plugins that return `None` add nothing.
    pub fn merged_router(&self, ctx: &AppContext) -> Router {
        let mut router = Router::new();
        for p in &self.plugins {
            if let Some(r) = p.routes(ctx) {
                router = router.merge(r);
            }
        }
        router
    }

    /// Merge every plugin's `admin_routes()`. Used by the CMS plugin to
    /// build the auth-gated admin surface — see the `Plugin::admin_routes`
    /// security invariant.
    pub fn merged_admin_router(&self, ctx: &AppContext) -> Router {
        let mut router = Router::new();
        for p in &self.plugins {
            if let Some(r) = p.admin_routes(ctx) {
                router = router.merge(r);
            }
        }
        router
    }

    /// Collect every plugin's `admin_nav()` entries, sorted by their
    /// declared `sort` value.
    pub fn merged_admin_nav(&self, ctx: &AppContext) -> Vec<AdminNavEntry> {
        let mut entries: Vec<AdminNavEntry> =
            self.plugins.iter().flat_map(|p| p.admin_nav(ctx)).collect();
        entries.sort_by_key(|e| (e.sort, e.label));
        entries
    }

    /// Names of all registered (= enabled) plugins.
    pub fn names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|p| p.name()).collect()
    }

    /// Compact rail rows for `/admin/api/plugins`. One row per registered
    /// plugin, in registration order. The Nuntius admin shell groups
    /// `primary == true` rows under "Broadcast" and the rest under "Plugins".
    pub fn rail_rows(&self, ctx: &AppContext) -> Vec<RailRow> {
        self.plugins
            .iter()
            .map(|p| {
                let st = p.status(ctx);
                RailRow {
                    id: p.id(),
                    name: p.name(),
                    kind: p.kind(),
                    icon: p.icon(),
                    primary: p.primary(),
                    state: st.state,
                    badge: st.badge,
                }
            })
            .collect()
    }

    /// Per-plugin status detail for `/admin/api/plugins/{id}/status`.
    pub fn plugin_status(&self, ctx: &AppContext, id: &str) -> Option<PluginStatus> {
        self.plugins
            .iter()
            .find(|p| p.id() == id)
            .map(|p| p.status(ctx))
    }

    /// Look up a registered plugin by its concrete type.
    ///
    /// Used by the host to fetch state held by a specific plugin (e.g. the
    /// CMS plugin's admin token) without round-tripping through config or
    /// shared globals.
    pub fn find<P: Plugin + 'static>(&self) -> Option<&P> {
        self.plugins
            .iter()
            .find_map(|p| p.as_any().downcast_ref::<P>())
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
