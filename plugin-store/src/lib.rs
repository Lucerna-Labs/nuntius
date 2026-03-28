use axum::{
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use ccc_core::{Plugin, PluginError};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::time::Duration;

/// The central GitHub repository URL hosting the official Creator Command Center (CCC) plugin registry.
/// Station operators will be able to submit PRs to this repository to add their plugins
/// to the global store without needing a centralized backend database.
const GITHUB_REGISTRY_URL: &str = "https://raw.githubusercontent.com/ccc/plugins/main/registry.json";

/// A representation of a plugin available in the store.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub is_installed: bool,
    pub download_url: Option<String>,
}

/// A wrapper struct for the JSON array returned by the GitHub raw file.
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginRegistry {
    pub plugins: Vec<StorePlugin>,
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
        "GitHub Plugin Store Manager"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing GitHub-backed Plugin Store API...");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Handler to list plugins dynamically fetched from the global GitHub registry.
async fn list_available_plugins() -> impl IntoResponse {
    let client = reqwest::Client::builder()
        // Provide a timeout so a slow connection doesn't lock up the API request
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    // Fetch the raw `registry.json` from the official GitHub repository.
    match client.get(GITHUB_REGISTRY_URL).send().await {
        Ok(response) if response.status().is_success() => {
            // Attempt to parse the JSON into our Rust structs.
            if let Ok(registry) = response.json::<PluginRegistry>().await {
                return Json(registry.plugins);
            }
        }
        _ => {} // Fallback on any error (404, network failure, etc.)
    }

    // Fallback Mock Data:
    // If the GitHub repository does not exist yet (or the user is offline),
    // return a basic "Coming Soon" payload so the UI does not break.
    let fallback = vec![
        StorePlugin {
            id: "ccc.plugin.chat".to_string(),
            name: "Live WebSockets Chat".to_string(),
            description: "A highly-concurrent, real-time chat box for your listeners. (Mock - Offline)".to_string(),
            version: "1.0.2".to_string(),
            is_installed: false,
            download_url: Some("https://registry.ccc.com/plugins/chat.wasm".to_string()),
        },
    ];

    Json(fallback)
}

/// Handler to list currently installed and active plugins.
async fn list_installed_plugins() -> impl IntoResponse {
    // In a full implementation, this would query the `PluginManager` from the core crate.
    let installed = vec![
        StorePlugin {
            id: "ccc.plugin.video".to_string(),
            name: "P2P Video Podcast Publisher".to_string(),
            description: "Broadcasting video streams over WebRTC to save server bandwidth.".to_string(),
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
        StorePlugin {
            id: "ccc.plugin.blog".to_string(),
            name: "Static Blog".to_string(),
            description: "A lightweight blog generator for your station.".to_string(),
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
    ];

    Json(installed)
}
