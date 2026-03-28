use axum::{
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use ccc_core::{Plugin, PluginError};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::time::Duration;

/// The central GitHub repository URL hosting the official 4C Plugin Registry.
const GITHUB_REGISTRY_URL: &str = "https://raw.githubusercontent.com/lucernamedia/ccc-plugins/main/registry.json";

/// The source classification for a plugin.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum PluginCategory {
    Official,
    Community,
}

/// A representation of a plugin available in the 4C Plugin Repository.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub category: PluginCategory,
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
        "4C GitHub Plugin Store Manager"
    }

    fn version(&self) -> &'static str {
        "0.3.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing 4C Plugin Store API...");
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

    // Fallback Mock Data representing the full 4C Blueprint ecosystem:
    let fallback = vec![
        // --- Official Plugins ---
        StorePlugin {
            id: "ccc.plugin.blog-static".to_string(),
            name: "Static Blog Generator".to_string(),
            description: "High performance static site generator for your station's articles.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/blog.wasm".to_string()),
        },
        StorePlugin {
            id: "ccc.plugin.newsletter-email".to_string(),
            name: "Newsletter Campaigns".to_string(),
            description: "Email automation and listener engagement campaigns.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/newsletter.wasm".to_string()),
        },
        StorePlugin {
            id: "ccc.plugin.activitypub".to_string(),
            name: "Fediverse Microblog".to_string(),
            description: "GoToSocial-style ActivityPub integration. Turn your command center into a federated node.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/activitypub.wasm".to_string()),
        },
        StorePlugin {
            id: "ccc.plugin.seo-manager".to_string(),
            name: "SEO Manager".to_string(),
            description: "Automatic meta tags, sitemaps, and search engine scoring.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/seo.wasm".to_string()),
        },
        StorePlugin {
            id: "ccc.plugin.analytics".to_string(),
            name: "Analytics Dashboard".to_string(),
            description: "Detailed listener and viewer statistics, heatmaps, and retention tracking.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/analytics.wasm".to_string()),
        },
        StorePlugin {
            id: "ccc.plugin.theme.dark-matter".to_string(),
            name: "Theme: Dark Matter".to_string(),
            description: "A gorgeous, customizable dark UI theme for the Command Center.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "1.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://registry.lucernamedia.com/plugins/themes/dark-matter.wasm".to_string()),
        },

        // --- Community Plugins ---
        StorePlugin {
            id: "com.community.monetization-patreon".to_string(),
            name: "Patreon Integration".to_string(),
            description: "Sync Patreon tiers to unlock premium ad-free streams and content for subscribers.".to_string(),
            author: "CreatorCommunity".to_string(),
            category: PluginCategory::Community,
            version: "0.9.5".to_string(),
            is_installed: false,
            download_url: Some("https://github.com/CreatorCommunity/ccc-patreon/releases/download/v0.9.5/plugin.wasm".to_string()),
        },
        StorePlugin {
            id: "com.community.monetization-ads".to_string(),
            name: "Ad Insertion Manager".to_string(),
            description: "Dynamic mid-roll ad insertion and sponsorship tracking for podcasts and streams.".to_string(),
            author: "AudioAdsInc".to_string(),
            category: PluginCategory::Community,
            version: "1.1.2".to_string(),
            is_installed: false,
            download_url: Some("https://github.com/AudioAdsInc/ccc-ads/releases/download/v1.1.2/plugin.wasm".to_string()),
        },
        StorePlugin {
            id: "com.community.transcription-whisper".to_string(),
            name: "Whisper Auto-Transcribe".to_string(),
            description: "Automatically transcribe podcast episodes to text using OpenAI's local Whisper model.".to_string(),
            author: "AIAudioHackers".to_string(),
            category: PluginCategory::Community,
            version: "2.0.0".to_string(),
            is_installed: false,
            download_url: Some("https://github.com/AIAudioHackers/ccc-whisper/releases/download/v2.0.0/plugin.wasm".to_string()),
        },
        StorePlugin {
            id: "com.community.backup-s3".to_string(),
            name: "S3 Cloud Backups".to_string(),
            description: "Automatically backup your CccDB and media assets to AWS S3, Cloudflare R2, or Backblaze.".to_string(),
            author: "CloudSafe".to_string(),
            category: PluginCategory::Community,
            version: "1.0.4".to_string(),
            is_installed: false,
            download_url: Some("https://github.com/CloudSafe/ccc-s3/releases/download/v1.0.4/plugin.wasm".to_string()),
        },
    ];

    Json(fallback)
}

/// Handler to list currently installed and active plugins.
async fn list_installed_plugins() -> impl IntoResponse {
    let installed = vec![
        StorePlugin {
            id: "ccc.plugin.video-podcast".to_string(),
            name: "Video Podcast Publisher".to_string(),
            description: "Broadcasting video streams over P2P WebRTC to drastically save server bandwidth.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
        StorePlugin {
            id: "ccc.plugin.voice-podcast".to_string(),
            name: "Voice Podcast Publisher".to_string(),
            description: "High-performance audio streaming, recording, and dynamic RSS generation.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
        StorePlugin {
            id: "ccc.plugin.live-radio".to_string(),
            name: "Live Radio Engine".to_string(),
            description: "Icecast and HLS native Rust streaming engine.".to_string(),
            author: "Lucerna Media".to_string(),
            category: PluginCategory::Official,
            version: "0.1.0".to_string(),
            is_installed: true,
            download_url: None,
        },
    ];

    Json(installed)
}
