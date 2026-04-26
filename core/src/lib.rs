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

/// Shared runtime context handed to every enabled plugin.
///
/// This is the single dependency injection seam between the host and plugins.
/// Add new fields here as the platform grows (config, http client, secrets).
/// Plugins ignore fields they don't use.
#[derive(Clone)]
pub struct AppContext {
    pub db: Arc<CccDB>,
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

    /// Names of all registered (= enabled) plugins.
    pub fn names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|p| p.name()).collect()
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
