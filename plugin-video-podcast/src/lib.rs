//! Video podcast plugin: upload + ffmpeg HLS transcode + browser playback.
//!
//! Public:
//! - GET /videos               HTML index for published on-demand video
//! - GET /videos/:slug         HTML page with HLS.js player
//! - GET /videos/hls/<dir>/*   ServeDir over the HLS output tree
//!
//! Admin:
//! - GET  /admin/videos                list (drafts + published)
//! - GET  /admin/videos/new            upload form
//! - POST /admin/videos/new            multipart create + queue transcode
//! - GET  /admin/videos/:id/edit       metadata editor
//! - POST /admin/videos/:id/edit       update + status flip
//! - POST /admin/videos/:id/delete     drop row + remove HLS dir + source file
//! - POST /admin/videos/:id/retry      re-queue transcode after a failure

use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_database::{CccDB, VideoPodcast, VideoStatus};
use ccc_plugin_utils::{
    format_clock, format_unix, human_bytes_fixed, render, sanitize_filename_ascii, unix_now,
};
use serde::Deserialize;
use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tower_http::{
    cors::{Any as CorsAny, CorsLayer},
    services::ServeDir,
};

const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024 * 1024; // 2 GB
const DEFAULT_HLS_DIR: &str = "./media/video-hls";
const DEFAULT_SOURCE_DIR: &str = "./media/video-source";
const DEFAULT_FFMPEG: &str = "ffmpeg";

// ============================================================================
// Plugin
// ============================================================================

pub struct VideoPodcastPlugin {
    pub hls_base_path: String,
    source_dir: PathBuf,
    ffmpeg: String,
}

impl VideoPodcastPlugin {
    pub fn new(hls_base_path: &str) -> Self {
        Self {
            hls_base_path: hls_base_path.to_string(),
            source_dir: PathBuf::from(DEFAULT_SOURCE_DIR),
            ffmpeg: DEFAULT_FFMPEG.to_string(),
        }
    }

    pub fn from_config(cfg: &ccc_core::CccConfig) -> Self {
        let table = cfg.plugin.get("video-podcast");
        let hls_dir = table
            .and_then(|t| t.get("hls_dir"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_HLS_DIR)
            .to_string();
        let source_dir = table
            .and_then(|t| t.get("source_dir"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOURCE_DIR));
        let ffmpeg = table
            .and_then(|t| t.get("ffmpeg"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_FFMPEG)
            .to_string();
        Self {
            hls_base_path: hls_dir,
            source_dir,
            ffmpeg,
        }
    }
}

#[derive(Clone)]
struct AppState {
    db: Arc<CccDB>,
    hls_dir: Arc<PathBuf>,
    source_dir: Arc<PathBuf>,
    ffmpeg: Arc<String>,
    /// One-shot guard for the background transcode worker. Replaces
    /// the old `static SPAWNED: AtomicBool` pattern that didn't work
    /// if you ever constructed two plugin instances (tests, hot-reload).
    transcode_worker_spawned: Arc<AtomicBool>,
}

impl Plugin for VideoPodcastPlugin {
    fn name(&self) -> &'static str {
        "Video Podcast"
    }

    fn version(&self) -> &'static str {
        "0.3.0"
    }

    fn id(&self) -> String {
        "video-podcast".into()
    }
    fn kind(&self) -> &'static str {
        "VIDEO"
    }
    fn icon(&self) -> &'static str {
        "icon-video"
    }
    fn admin_url(&self) -> String {
        "/admin/videos".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let avail = ffmpeg_available(&self.ffmpeg);
        PluginStatus {
            state: if avail {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": avail,
                "ffmpeg": *self.ffmpeg,
                "hls_dir": self.hls_base_path,
                "source_dir": self.source_dir.to_string_lossy(),
                "missing": if avail { Vec::<String>::new() } else { vec!["ffmpeg binary on PATH".to_string()] },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        let _ = std::fs::create_dir_all(&self.hls_base_path);
        let _ = std::fs::create_dir_all(&self.source_dir);
        let avail = ffmpeg_available(&self.ffmpeg);
        println!(
            "Initializing Video Podcast Plugin (HLS: {}, source: {}, ffmpeg: {})",
            self.hls_base_path,
            self.source_dir.display(),
            if avail { "found" } else { "NOT found" }
        );
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        let cors = CorsLayer::new()
            .allow_origin(CorsAny)
            .allow_methods(CorsAny)
            .allow_headers(CorsAny);
        let state = AppState {
            db: ctx.db.clone(),
            hls_dir: Arc::new(PathBuf::from(&self.hls_base_path)),
            source_dir: Arc::new(self.source_dir.clone()),
            ffmpeg: Arc::new(self.ffmpeg.clone()),
            transcode_worker_spawned: Arc::new(AtomicBool::new(false)),
        };
        Some(
            Router::new()
                .route("/videos", get(public_index))
                .route("/videos/:slug", get(public_show))
                .nest_service(
                    "/videos/hls",
                    ServeDir::new(PathBuf::from(&self.hls_base_path)),
                )
                .with_state(state)
                .layer(cors),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AppState {
            db: ctx.db.clone(),
            hls_dir: Arc::new(PathBuf::from(&self.hls_base_path)),
            source_dir: Arc::new(self.source_dir.clone()),
            ffmpeg: Arc::new(self.ffmpeg.clone()),
            transcode_worker_spawned: Arc::new(AtomicBool::new(false)),
        };
        spawn_transcode_worker(state.clone());
        Some(
            Router::new()
                .route("/videos", get(admin_list))
                .route("/videos/new", get(admin_new_form).post(admin_create))
                .route("/videos/:id/edit", get(admin_edit_form).post(admin_update))
                .route("/videos/:id/delete", post(admin_delete))
                .route("/videos/:id/retry", post(admin_retry))
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
                .with_state(state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Video",
            href: "/admin/videos".into(),
            icon: "🎥",
            sort: 120,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Background transcode worker
// ============================================================================

fn ffmpeg_available(name: &str) -> bool {
    use std::process::Command;
    Command::new(name)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn spawn_transcode_worker(state: AppState) {
    // Per-plugin one-shot guard. Replaces the old `static SPAWNED`
    // pattern that didn't work if you ever constructed two plugin
    // instances (tests, hot-reload, multi-tenant).
    if state.transcode_worker_spawned.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if !ffmpeg_available(&state.ffmpeg) {
                continue;
            }
            let pending = match state.db.videos_pending_transcode() {
                Ok(v) => v,
                Err(_) => continue,
            };
            for v in pending {
                transcode_one(&state, v).await;
            }
        }
    });
}

/// Adaptive-bitrate ladder rendered for every uploaded video.
/// (height, video bitrate, audio bitrate). Tuned for talking-head /
/// podcast video, not 4K cinema. The HLS player serves the highest
/// quality the listener's connection can sustain.
const VARIANTS: &[(u32, &str, &str)] = &[
    (240, "400k", "64k"),
    (360, "800k", "96k"),
    (480, "1400k", "128k"),
    (720, "2800k", "128k"),
];

async fn transcode_one(state: &AppState, mut video: VideoPodcast) {
    use tokio::process::Command;

    let hls_dir = state.hls_dir.as_ref().join(&video.hls_directory);
    if let Err(e) = std::fs::create_dir_all(&hls_dir) {
        finish_failed(state, video, format!("could not create hls dir: {}", e));
        return;
    }

    // Single ffmpeg invocation that produces:
    //   <hls_dir>/v0/playlist.m3u8 (240p)
    //   <hls_dir>/v1/playlist.m3u8 (360p)
    //   <hls_dir>/v2/playlist.m3u8 (480p)
    //   <hls_dir>/v3/playlist.m3u8 (720p)
    //   <hls_dir>/master.m3u8 (referencing all four)
    // Doing it in one process is faster and saves a re-decode per rung.
    let mut args: Vec<String> = vec!["-y".into(), "-i".into(), video.source_path.clone()];
    // Per-variant codec settings. Use stream specifiers (`:N`) on `-b:v` /
    // `-b:a` so each output gets its OWN bitrate; without the specifier,
    // ffmpeg applies the last value to all four variants and 240p ends up
    // encoded at 2800kbps.
    for (i, (_h, vbr, abr)) in VARIANTS.iter().enumerate() {
        args.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-c:a".into(),
            "aac".into(),
            format!("-b:v:{}", i),
            (*vbr).to_string(),
            format!("-b:a:{}", i),
            (*abr).to_string(),
        ]);
    }
    // Map video + (optional) audio into each output, scaled per height.
    for (i, (h, _vbr, _abr)) in VARIANTS.iter().enumerate() {
        args.extend([
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a:0?".into(),
            format!("-s:v:{}", i),
            format!("{}x{}", (*h * 16) / 9, h),
        ]);
    }
    let var_stream_map = (0..VARIANTS.len())
        .map(|i| format!("v:{},a:{}", i, i))
        .collect::<Vec<_>>()
        .join(" ");
    args.extend([
        "-var_stream_map".into(),
        var_stream_map,
        "-master_pl_name".into(),
        "master.m3u8".into(),
        "-f".into(),
        "hls".into(),
        "-hls_time".into(),
        "6".into(),
        "-hls_list_size".into(),
        "0".into(),
        "-hls_segment_filename".into(),
        hls_dir
            .join("v%v/seg-%03d.ts")
            .to_string_lossy()
            .to_string(),
        hls_dir
            .join("v%v/playlist.m3u8")
            .to_string_lossy()
            .to_string(),
    ]);

    let cmd = Command::new(state.ffmpeg.as_ref())
        .args(&args)
        .output()
        .await;

    match cmd {
        Ok(o) if o.status.success() => {
            video.status = VideoStatus::Draft;
            video.error = None;
            video.updated_at = unix_now();
            let _ = state.db.upsert_video_podcast(&video);
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
            // Trim — ffmpeg stderr can be a novel
            let trimmed = if stderr.len() > 800 {
                format!("...{}", &stderr[stderr.len() - 800..])
            } else {
                stderr
            };
            finish_failed(
                state,
                video,
                format!("ffmpeg exit {}: {}", o.status, trimmed),
            );
        }
        Err(e) => finish_failed(state, video, format!("ffmpeg spawn failed: {}", e)),
    }
}

fn finish_failed(state: &AppState, mut video: VideoPodcast, err: String) {
    video.status = VideoStatus::Failed;
    video.error = Some(err);
    video.updated_at = unix_now();
    let _ = state.db.upsert_video_podcast(&video);
}

// ============================================================================
// Public handlers
// ============================================================================

#[derive(Template)]
#[template(path = "public_index.html")]
struct PublicIndexPage {
    videos: Vec<PublicVideoRow>,
    upcoming: Vec<PublicVideoRow>,
}

struct PublicVideoRow {
    title: String,
    slug: String,
    description: String,
    duration: String,
    published_human: String,
    status: &'static str,
}

async fn public_index(State(state): State<AppState>) -> Response {
    let published = match state.db.latest_published_video_podcasts(100) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let videos = published.into_iter().map(public_video_row).collect();

    let all = match state.db.all_video_podcasts() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let upcoming = all
        .into_iter()
        .filter(|v| v.status == VideoStatus::Draft || v.status == VideoStatus::Transcoding)
        .take(6)
        .map(public_video_row)
        .collect();

    render(PublicIndexPage { videos, upcoming })
}

fn public_video_row(v: VideoPodcast) -> PublicVideoRow {
    PublicVideoRow {
        title: v.title,
        slug: v.slug,
        description: v.description,
        duration: format_clock(v.duration_seconds),
        published_human: v
            .published_at
            .map(format_unix)
            .unwrap_or_else(|| "Coming soon".into()),
        status: v.status.as_str(),
    }
}

#[derive(Template)]
#[template(path = "public_show.html")]
struct PublicShowPage {
    title: String,
    description: String,
    hls_dir: String,
    duration: String,
    published_human: String,
}

async fn public_show(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let v = match state.db.get_video_podcast_by_slug(&slug) {
        Ok(Some(x)) if x.status == VideoStatus::Published => x,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicShowPage {
        title: v.title,
        description: v.description,
        hls_dir: v.hls_directory,
        duration: format_clock(v.duration_seconds),
        published_human: format_unix(v.published_at.unwrap_or(v.updated_at)),
    })
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage {
    videos: Vec<VideoRow>,
    ffmpeg_available: bool,
}

struct VideoRow {
    id: u64,
    title: String,
    slug: String,
    status_str: &'static str,
    size_human: String,
    updated_human: String,
}

async fn admin_list(State(state): State<AppState>) -> Response {
    let raw = match state.db.all_video_podcasts() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Vec<VideoRow> = raw
        .into_iter()
        .map(|v| VideoRow {
            id: v.id,
            title: v.title,
            slug: v.slug,
            status_str: v.status.as_str(),
            size_human: human_bytes_fixed(v.size_bytes),
            updated_human: format_unix(v.updated_at),
        })
        .collect();
    render(AdminListPage {
        videos: rows,
        ffmpeg_available: ffmpeg_available(&state.ffmpeg),
    })
}

#[derive(Template)]
#[template(path = "admin_new.html")]
struct AdminNewPage<'a> {
    error: Option<&'a str>,
    max_mb: usize,
}

async fn admin_new_form() -> Response {
    render(AdminNewPage {
        error: None,
        max_mb: MAX_UPLOAD_BYTES / (1024 * 1024),
    })
}

async fn admin_create(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let mut title = String::new();
    let mut slug = String::new();
    let mut description = String::new();
    let mut source_path: Option<PathBuf> = None;
    let mut mime: Option<String> = None;
    let mut size: u64 = 0;
    let now = unix_now();

    if let Err(e) = std::fs::create_dir_all(state.source_dir.as_ref()) {
        return new_form_error(&format!("Could not prepare source directory: {}", e));
    }

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return new_form_error(&format!("Multipart error: {}", e)),
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "title" => title = field.text().await.unwrap_or_default(),
            "slug" => slug = field.text().await.unwrap_or_default(),
            "description" => description = field.text().await.unwrap_or_default(),
            "video" => {
                let original = field.file_name().unwrap_or("upload.bin").to_string();
                let detected_mime = field
                    .content_type()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "video/mp4".into());
                let bytes = match field.bytes().await {
                    Ok(b) => b,
                    Err(e) => return new_form_error(&format!("Upload read error: {}", e)),
                };
                let safe = format!("{}-{}", now, sanitize_filename_ascii(&original, "video.bin"));
                let mut path = state.source_dir.as_ref().clone();
                path.push(&safe);
                let mut f = match tokio::fs::File::create(&path).await {
                    Ok(f) => f,
                    Err(e) => return new_form_error(&format!("Could not write video: {}", e)),
                };
                if let Err(e) = f.write_all(&bytes).await {
                    return new_form_error(&format!("Disk write error: {}", e));
                }
                let _ = f.flush().await;
                source_path = Some(path);
                mime = Some(detected_mime);
                size = bytes.len() as u64;
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let title = title.trim().to_string();
    let slug = slug.trim().to_string();
    if title.is_empty() {
        return new_form_error("Title is required.");
    }
    if slug.is_empty() {
        return new_form_error("Slug is required.");
    }
    let source_path = match source_path {
        Some(p) => p,
        None => return new_form_error("Video file is required."),
    };
    if size == 0 {
        let _ = std::fs::remove_file(&source_path);
        return new_form_error("The video file was empty.");
    }

    let video_id = match state.db.next_id("video_podcast") {
        Ok(id) => id,
        Err(e) => return new_form_error(&format!("Could not allocate video id: {e}")),
    };
    let hls_dir_name = format!("{}-{}", video_id, slug);
    let video = VideoPodcast {
        id: video_id,
        slug,
        title,
        description,
        source_path: source_path.to_string_lossy().to_string(),
        hls_directory: hls_dir_name,
        mime_type: mime.unwrap_or_else(|| "video/mp4".into()),
        size_bytes: size,
        duration_seconds: 0,
        status: VideoStatus::Transcoding,
        error: None,
        created_at: now,
        updated_at: now,
        published_at: None,
    };
    if let Err(e) = state.db.upsert_video_podcast(&video) {
        let _ = std::fs::remove_file(&source_path);
        return new_form_error(&format!("Could not save video: {}", e));
    }
    Redirect::to("/admin/videos").into_response()
}

#[derive(Template)]
#[template(path = "admin_edit.html")]
struct AdminEditPage<'a> {
    id: u64,
    title: String,
    slug: String,
    description: String,
    status_str: &'static str,
    source_path: String,
    hls_directory: String,
    size_human: String,
    created_human: String,
    updated_human: String,
    error: Option<&'a str>,
    transcode_error: Option<String>,
}

async fn admin_edit_form(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let v = match state.db.get_video_podcast(id) {
        Ok(Some(x)) => x,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(AdminEditPage {
        id: v.id,
        title: v.title,
        slug: v.slug,
        description: v.description,
        status_str: v.status.as_str(),
        source_path: v.source_path,
        hls_directory: v.hls_directory,
        size_human: human_bytes_fixed(v.size_bytes),
        created_human: format_unix(v.created_at),
        updated_human: format_unix(v.updated_at),
        error: None,
        transcode_error: v.error,
    })
}

#[derive(Deserialize)]
struct EditForm {
    title: String,
    slug: String,
    description: String,
    #[serde(default)]
    status: String,
}

async fn admin_update(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<EditForm>,
) -> Response {
    let mut v = match state.db.get_video_podcast(id) {
        Ok(Some(x)) => x,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = unix_now();
    let was_published = v.status == VideoStatus::Published;
    v.title = form.title.trim().to_string();
    v.slug = form.slug.trim().to_string();
    v.description = form.description;
    v.updated_at = now;
    // Don't allow publishing a Transcoding/Failed video.
    if v.status == VideoStatus::Draft || v.status == VideoStatus::Published {
        match form.status.as_str() {
            "published" => {
                v.status = VideoStatus::Published;
                if !was_published {
                    v.published_at = Some(now);
                }
            }
            _ => {
                v.status = VideoStatus::Draft;
                v.published_at = None;
            }
        }
    }
    let _ = state.db.upsert_video_podcast(&v);
    Redirect::to("/admin/videos").into_response()
}

async fn admin_delete(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    if let Ok(Some(v)) = state.db.get_video_podcast(id) {
        let _ = std::fs::remove_file(&v.source_path);
        let mut hls_path = state.hls_dir.as_ref().clone();
        hls_path.push(&v.hls_directory);
        let _ = std::fs::remove_dir_all(&hls_path);
    }
    let _ = state.db.delete_video_podcast(id);
    Redirect::to("/admin/videos").into_response()
}

async fn admin_retry(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    if let Ok(Some(mut v)) = state.db.get_video_podcast(id) {
        v.status = VideoStatus::Transcoding;
        v.error = None;
        v.updated_at = unix_now();
        let _ = state.db.upsert_video_podcast(&v);
    }
    Redirect::to(&format!("/admin/videos/{}/edit", id)).into_response()
}

// ============================================================================
// Helpers
// ============================================================================
// The generic ones (render, human_bytes_fixed, sanitize_filename_ascii,
// format_clock, unix_now, format_unix) live in ccc-plugin-utils; only
// video-specific glue stays here.

fn new_form_error(msg: &str) -> Response {
    render(AdminNewPage {
        error: Some(msg),
        max_mb: MAX_UPLOAD_BYTES / (1024 * 1024),
    })
}
