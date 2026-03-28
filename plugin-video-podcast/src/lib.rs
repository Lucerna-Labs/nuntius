use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use ccc_core::{Plugin, PluginError};
use ccc_database::CccDB;
use std::any::Any;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tower_http::cors::{Any as CorsAny, CorsLayer};

/// Shared state for the Video Podcast plugin.
struct AppState {
    db: Arc<CccDB>,
}

/// The main entry point for the Video Podcast Plugin.
pub struct VideoPodcastPlugin {
    pub hls_base_path: String,
}

impl VideoPodcastPlugin {
    pub fn new(hls_base_path: &str) -> Self {
        Self {
            hls_base_path: hls_base_path.to_string(),
        }
    }

    /// Build the `axum` router for the video podcast endpoints.
    pub fn build_router(&self, db: Arc<CccDB>) -> Router {
        let state = Arc::new(AppState { db });

        // We must allow CORS so the P2P WebRTC WebTorrent trackers and other
        // viewers can exchange segments freely.
        let cors = CorsLayer::new()
            .allow_origin(CorsAny)
            .allow_methods(CorsAny)
            .allow_headers(CorsAny);

        Router::new()
            .route("/video/:id/player", get(video_player_handler))
            // Serve the raw HLS directories. `ServeDir` automatically handles
            // Range Requests and efficient file streaming for .m3u8 and .ts files.
            .nest_service("/video/hls", ServeDir::new(&self.hls_base_path))
            .with_state(state)
            .layer(cors)
    }
}

impl Plugin for VideoPodcastPlugin {
    fn name(&self) -> &'static str {
        "P2P Video Podcast Publisher"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing Video Podcast Plugin (HLS Directory: {})", self.hls_base_path);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Handler to generate the HTML player with P2P Media Loader embedded.
async fn video_player_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<Html<String>, StatusCode> {
    let video = state
        .db
        .get_video_podcast(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // This HTML uses hls.js wrapped with p2p-media-loader-core and p2p-media-loader-hlsjs.
    // It intercepts the HLS segment requests and attempts to fetch them from WebRTC peers first,
    // drastically reducing strain on this Creator Command Center (CCC) server.
    let html = format!(
        r#"
        <!DOCTYPE html>
        <html lang="en">
        <head>
            <meta charset="UTF-8">
            <title>{} - Creator Command Center (CCC) Video</title>
            <script src="https://cdn.jsdelivr.net/npm/hls.js@latest"></script>
            <script src="https://cdn.jsdelivr.net/npm/p2p-media-loader-core@latest/build/p2p-media-loader-core.min.js"></script>
            <script src="https://cdn.jsdelivr.net/npm/p2p-media-loader-hlsjs@latest/build/p2p-media-loader-hlsjs.min.js"></script>
            <style>
                body {{ background: #111; color: #fff; font-family: sans-serif; text-align: center; }}
                video {{ width: 80%; max-width: 1080px; margin-top: 20px; box-shadow: 0 4px 15px rgba(0,0,0,0.5); }}
                #stats {{ margin-top: 10px; font-size: 0.9em; color: #aaa; }}
            </style>
        </head>
        <body>
            <h1>{}</h1>
            <p>{}</p>
            <video id="video" controls></video>
            <div id="stats">P2P Engine Initializing...</div>

            <script>
                if (Hls.isSupported() && p2pml.hlsjs.Engine.isSupported()) {{
                    var engine = new p2pml.hlsjs.Engine();

                    var hls = new Hls({{
                        liveSyncDurationCount: 7,
                        loader: engine.createLoaderClass()
                    }});

                    p2pml.hlsjs.initHlsJsPlayer(hls);

                    var video = document.getElementById('video');
                    // The HLS playlist is served by our Axum `ServeDir` setup.
                    // E.g., /video/hls/[hls_directory_name]/playlist.m3u8
                    hls.loadSource('/video/hls/{}/playlist.m3u8');
                    hls.attachMedia(video);

                    engine.on(p2pml.core.Events.PieceBytesDownloaded, function (method, size) {{
                        document.getElementById('stats').innerText = 'Downloaded ' + size + ' bytes via ' + method;
                    }});
                }} else {{
                    document.getElementById('stats').innerText = 'Your browser does not support HLS or P2P.';
                }}
            </script>
        </body>
        </html>
        "#,
        video.title, video.title, video.description, video.hls_directory
    );

    Ok(Html(html))
}
