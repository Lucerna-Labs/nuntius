use axum::{
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use azuracast_core::{Plugin, PluginError};
use serde::{Deserialize, Serialize};
use std::any::Any;

/// A representation of a plugin available in the store.
/// In the future, this metadata would be fetched from a central registry,
/// and the `download_url` would point to a WebAssembly (WASM) module
/// that our Rust core executes safely in a sandbox.
#[derive(Debug, Serialize, Deserialize)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub is_installed: bool,
    pub download_url: Option<String>,
}

/// The main entry point for the Plugin Store manager.
pub struct PluginStorePlugin;

impl PluginStorePlugin {
    pub fn new() -> Self {
        Self
    }

    /// Build the `axum` router for the plugin store endpoints.
    pub fn build_router() -> Router {
        Router::new()
            .route("/api/plugins/store", get(list_available_plugins))
            .route("/api/plugins/installed", get(list_installed_plugins))
    }
}

impl Plugin for PluginStorePlugin {
    fn name(&self) -> &'static str {
        "Plugin Store Manager"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing Plugin Store API...");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Handler to list plugins available for download from the central registry.
async fn list_available_plugins() -> impl IntoResponse {
    // Mock data for the storefront.
    let available = vec![
        StorePlugin {
            id: "azuracast.plugin.chat".to_string(),
            name: "Live WebSockets Chat".to_string(),
            description: "A highly-concurrent, real-time chat box for your listeners.".to_string(),
            version: "1.0.2".to_string(),
            is_installed: false,
            download_url: Some("https://registry.azuracast.com/plugins/chat.wasm".to_string()),
        },
        StorePlugin {
            id: "azuracast.plugin.analytics".to_string(),
            name: "Advanced Listener Analytics".to_string(),
            description: "Heatmaps and geographic listener tracking.".to_string(),
            version: "0.9.1".to_string(),
            is_installed: false,
            download_url: Some("https://registry.azuracast.com/plugins/analytics.wasm".to_string()),
        },
    ];

    Json(available)
}

/// Handler to list currently installed and active plugins.
async fn list_installed_plugins() -> impl IntoResponse {
    // In a full implementation, this would query the `PluginManager` from the core crate.
    let installed = vec![
        StorePlugin {
            id: "azuracast.plugin.video".to_string(),
            name: "P2P Video Podcast Publisher".to_string(),
            description: "Broadcasting video streams over WebRTC to save server bandwidth.".to_string(),
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
        StorePlugin {
            id: "azuracast.plugin.blog".to_string(),
            name: "Static Blog".to_string(),
            description: "A lightweight blog generator for your station.".to_string(),
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
    ];

    Json(installed)
}
