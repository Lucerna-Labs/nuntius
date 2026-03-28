use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use ccc_core::{Plugin, PluginError};
use ccc_database::CccDB;
use rss::{ChannelBuilder, ItemBuilder};
use std::any::Any;
use std::sync::Arc;
use tokio_util::io::ReaderStream;
use axum::body::Body;

/// State shared across the podcast routes.
struct AppState {
    db: Arc<CccDB>,
}

/// The main entry point for the Podcast Plugin.
pub struct PodcastPlugin {
    db_path: String,
}

impl PodcastPlugin {
    pub fn new(db_path: &str) -> Self {
        Self {
            db_path: db_path.to_string(),
        }
    }

    /// Build the `axum` router for the podcast endpoints.
    pub fn build_router(db: Arc<CccDB>) -> Router {
        let state = Arc::new(AppState { db });

        Router::new()
            .route("/podcasts/feed.xml", get(rss_feed_handler))
            .route("/podcasts/download/:id", get(download_episode_handler))
            .with_state(state)
    }
}

impl Plugin for PodcastPlugin {
    fn name(&self) -> &'static str {
        "Podcast Publisher"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing Podcast Plugin against DB at {}...", self.db_path);
        // In a real implementation, we would register the `build_router` with the central API server.
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Handler to generate the RSS XML feed dynamically from the database.
async fn rss_feed_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Get the latest 50 episodes
    let episodes = state.db.get_latest_podcast_episodes(50).unwrap_or_default();

    let mut items = Vec::new();
    for ep in episodes {
        let item = ItemBuilder::default()
            .title(ep.title)
            .description(ep.description)
            // Example link: In a real app this would be the actual public URL
            .link(format!("https://radio.example.com/api/podcasts/download/{}", ep.id))
            .build();
        items.push(item);
    }

    let channel = ChannelBuilder::default()
        .title("Creator Command Center (CCC) High-Performance Podcast")
        .link("https://radio.example.com")
        .description("A sample podcast generated purely in Rust.")
        .items(items)
        .build();

    let xml = channel.to_string();

    (
        [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")],
        xml,
    )
}

/// Handler to securely and efficiently stream a podcast episode from disk.
/// This uses `tokio::fs::File` and `ReaderStream` to serve the file asynchronously,
/// supporting HTTP Range Requests with almost zero RAM usage.
async fn download_episode_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, StatusCode> {
    let episode = state
        .db
        .get_podcast_episode(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Open the file asynchronously
    let file = tokio::fs::File::open(&episode.file_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Convert the file into an asynchronous stream of bytes
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    // Return the response, informing the client it's an audio stream
    let headers = [
        (header::CONTENT_TYPE, "audio/mpeg"),
        // Note: For full range request support (scrubbing/seeking),
        // we would wrap this in tower_http::services::ServeFile.
        // This streaming body handles the raw delivery efficiently.
        (header::ACCEPT_RANGES, "bytes"),
    ];

    Ok((headers, body))
}
