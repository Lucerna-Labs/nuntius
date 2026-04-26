//! Core functionality for the Creator Command Center (CCC) Rust rewrite.
//!
//! This crate contains the essential building blocks of the application,
//! including the plugin architecture, fundamental data structures, and traits.

pub mod activitypub;
pub mod config;

pub use config::{CccConfig, ConfigError, DatabaseConfig, PluginsConfig, ServerConfig};

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

/// A trait that all plugins must implement to hook into the Creator Command Center (CCC) core.
///
/// **Lifecycle contract:** if a plugin is *disabled* in `ccc.toml`, it is
/// never constructed — `new()` is not called, no `Plugin` value exists, no
/// memory is allocated, no router is registered, no background task spawned.
/// "Disabled" means *absent*, not "idle."
pub trait Plugin: Send + Sync {
    /// Returns the name of the plugin.
    fn name(&self) -> &'static str;

    /// Returns the version of the plugin.
    fn version(&self) -> &'static str;

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

    /// Look up a registered plugin by its concrete type.
    ///
    /// Used by the host to fetch state held by a specific plugin (e.g. the
    /// CMS plugin's admin token) without round-tripping through config or
    /// shared globals.
    pub fn find<P: Plugin + 'static>(&self) -> Option<&P> {
        self.plugins.iter().find_map(|p| p.as_any().downcast_ref::<P>())
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
