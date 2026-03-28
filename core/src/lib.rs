//! Core functionality for the Creator Command Center (CCC) Rust rewrite.
//!
//! This crate contains the essential building blocks of the application,
//! including the plugin architecture, fundamental data structures, and traits.

use std::any::Any;

/// A trait that all plugins must implement to hook into the Creator Command Center (CCC) core.
pub trait Plugin: Send + Sync {
    /// Returns the name of the plugin.
    fn name(&self) -> &'static str;

    /// Returns the version of the plugin.
    fn version(&self) -> &'static str;

    /// Initialize the plugin. This is called once when the plugin is loaded.
    ///
    /// It can be used to set up routes, initialize state, or register extensions.
    fn init(&mut self) -> Result<(), PluginError>;

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

    /// Register a new plugin.
    pub fn register<P: Plugin + 'static>(&mut self, mut plugin: P) -> Result<(), PluginError> {
        plugin.init()?;
        self.plugins.push(Box::new(plugin));
        Ok(())
    }

    /// Return an iterator over registered plugins.
    pub fn plugins(&self) -> impl Iterator<Item = &Box<dyn Plugin>> {
        self.plugins.iter()
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
