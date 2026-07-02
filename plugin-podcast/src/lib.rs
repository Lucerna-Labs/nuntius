//! Podcast plugin: episode CRUD, multipart upload, RSS feed,
//! optional auto-queueing of Whisper transcription jobs.
//!
//! Public:
//! - GET  /podcasts/feed.xml         RSS 2.0 + iTunes namespace, only Published
//! - GET  /podcasts/                 HTML index for published on-demand episodes
//! - GET  /podcasts/:slug            HTML page for one episode
//! - GET  /podcasts/download/:id     audio stream (Range-friendly via ServeFile)
//!
//! Admin:
//! - GET  /admin/podcasts                 list (drafts + published)
//! - GET  /admin/podcasts/new             upload form
//! - POST /admin/podcasts/new             multipart create
//! - GET  /admin/podcasts/:id/edit        edit form
//! - POST /admin/podcasts/:id/edit        update metadata
//! - POST /admin/podcasts/:id/delete      drop row + delete file

use askama::Template;
use axum::body::Body;
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError, PluginLifecycle, PluginStatus,
    ServiceRegistry,
};
use ccc_database::{CccDB, EpisodeStatus, PodcastEpisode, TranscriptionJob, TranscriptionStatus};
use ccc_plugin_utils::{
    format_clock, format_unix, human_bytes_fixed, render, rfc2822_utc, sanitize_filename_ascii,
    unix_now,
};
use rss::{
    extension::itunes::{ITunesChannelExtensionBuilder, ITunesItemExtensionBuilder},
    ChannelBuilder, EnclosureBuilder, GuidBuilder, ItemBuilder,
};
use serde::Deserialize;
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const MAX_UPLOAD_BYTES: usize = 500 * 1024 * 1024; // 500 MB
const DEFAULT_AUDIO_DIR: &str = "./media/podcast";

/// Default for the members-only feed token. Operators MUST override this in
/// ccc.toml ([plugin.podcast].members_token) before announcing the URL.
const DEFAULT_MEMBERS_TOKEN: &str = "change-me";

// ============================================================================
// Plugin
// ============================================================================

pub struct PodcastPlugin {
    audio_dir: PathBuf,
    members_token: String,
    /// Operator-configured site name (`[site] name` in ccc.toml). Used to
    /// brand public pages and the RSS channel as THIS deployment; the
    /// "Powered by Nuntius" footer stays platform attribution.
    site_name: String,
}

impl PodcastPlugin {
    pub fn new(audio_dir: &str) -> Self {
        Self {
            audio_dir: PathBuf::from(audio_dir),
            members_token: DEFAULT_MEMBERS_TOKEN.to_string(),
            site_name: "Nuntius".to_string(),
        }
    }

    pub fn from_config(cfg: &ccc_core::CccConfig) -> Self {
        let table = cfg.plugin.get("podcast");
        let audio_dir = table
            .and_then(|t| t.get("audio_dir"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_AUDIO_DIR)
            .to_string();
        let members_token = table
            .and_then(|t| t.get("members_token"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_MEMBERS_TOKEN)
            .to_string();
        Self {
            audio_dir: PathBuf::from(audio_dir),
            members_token,
            site_name: cfg.site.name.clone(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    db: Arc<CccDB>,
    audio_dir: Arc<PathBuf>,
    members_token: Arc<String>,
    site_name: Arc<String>,
    events: Arc<EventBus>,
    services: Arc<ServiceRegistry>,
}

impl Plugin for PodcastPlugin {
    fn name(&self) -> &'static str {
        "Podcast"
    }

    fn version(&self) -> &'static str {
        "0.3.0"
    }

    fn id(&self) -> String {
        "podcast".into()
    }
    fn kind(&self) -> &'static str {
        "AUDIO"
    }
    fn icon(&self) -> &'static str {
        "icon-podcast"
    }
    fn admin_url(&self) -> String {
        "/admin/podcasts".into()
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let episodes = ctx.db.all_podcast_episodes().unwrap_or_default();
        let rows: Vec<_> = episodes
            .iter()
            .rev()
            .take(10)
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "title": e.title,
                    "status": if e.published_at.is_some() { "published" } else { "producing" },
                    "duration": format_clock(e.duration_seconds),
                    "downloads": 0u64,
                })
            })
            .collect();
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: None,
            detail: serde_json::json!({
                "configured": true,
                "audio_dir": self.audio_dir.to_string_lossy(),
                "members_token_set": self.members_token != "change-me",
                "episodes": rows,
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        if let Err(e) = std::fs::create_dir_all(&self.audio_dir) {
            return Err(PluginError::InitError(format!(
                "podcast: could not create audio dir {}: {}",
                self.audio_dir.display(),
                e
            )));
        }
        println!(
            "Initializing Podcast Plugin (audio dir: {})",
            self.audio_dir.display()
        );
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AppState {
            db: ctx.db.clone(),
            audio_dir: Arc::new(self.audio_dir.clone()),
            members_token: Arc::new(self.members_token.clone()),
            site_name: Arc::new(self.site_name.clone()),
            events: ctx.events.clone(),
            services: ctx.services.clone(),
        };
        Some(
            Router::new()
                .route("/podcasts", get(public_index))
                .route("/podcasts/feed.xml", get(public_feed))
                .route("/podcasts/members.xml", get(public_members_feed))
                .route("/podcasts/download/:id", get(public_download))
                .route("/podcasts/:slug", get(public_show))
                .with_state(state),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AppState {
            db: ctx.db.clone(),
            audio_dir: Arc::new(self.audio_dir.clone()),
            members_token: Arc::new(self.members_token.clone()),
            site_name: Arc::new(self.site_name.clone()),
            events: ctx.events.clone(),
            services: ctx.services.clone(),
        };
        Some(
            Router::new()
                .route("/podcasts", get(admin_list))
                .route("/podcasts/new", get(admin_new_form).post(admin_create))
                .route(
                    "/podcasts/:id/edit",
                    get(admin_edit_form).post(admin_update),
                )
                .route("/podcasts/:id/delete", post(admin_delete))
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
                .with_state(state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Podcast",
            href: "/admin/podcasts".into(),
            icon: "🎙️",
            sort: 110,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Public handlers
// ============================================================================

#[derive(Template)]
#[template(path = "public_index.html")]
struct PublicIndexPage {
    site_name: String,
    episodes: Vec<PublicEpisodeRow>,
    upcoming: Vec<PublicEpisodeRow>,
}

struct PublicEpisodeRow {
    id: u64,
    title: String,
    slug: String,
    description: String,
    duration: String,
    published_human: String,
}

async fn public_index(State(state): State<AppState>) -> Response {
    let published = match state.db.latest_published_podcast_episodes(100) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let episodes = published
        .into_iter()
        .filter(|e| !e.members_only)
        .map(public_episode_row)
        .collect();

    let all = match state.db.all_podcast_episodes() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let upcoming = all
        .into_iter()
        .filter(|e| e.status == EpisodeStatus::Draft && !e.members_only)
        .take(6)
        .map(public_episode_row)
        .collect();

    render(PublicIndexPage {
        site_name: state.site_name.as_ref().clone(),
        episodes,
        upcoming,
    })
}

fn public_episode_row(ep: PodcastEpisode) -> PublicEpisodeRow {
    PublicEpisodeRow {
        id: ep.id,
        title: ep.title,
        slug: ep.slug,
        description: ep.description,
        duration: format_clock(ep.duration_seconds),
        published_human: ep
            .published_at
            .map(format_unix)
            .unwrap_or_else(|| "Coming soon".into()),
    }
}

async fn public_feed(State(state): State<AppState>) -> Response {
    let raw = match state.db.latest_published_podcast_episodes(50) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let episodes: Vec<_> = raw.into_iter().filter(|e| !e.members_only).collect();
    build_feed_response(
        &format!("{} Podcast", state.site_name),
        &state.site_name,
        &episodes,
    )
}

#[derive(Deserialize)]
struct MembersFeedQuery {
    token: Option<String>,
}

async fn public_members_feed(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<MembersFeedQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !members_authorized(&state, &q, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let episodes = match state.db.latest_published_podcast_episodes(50) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    build_feed_response(
        &format!("{} Podcast (Members)", state.site_name),
        &state.site_name,
        &episodes,
    )
}

/// Members are admitted if EITHER:
/// - the request carries the configured shared `?token=...`, OR
/// - a membership provider is active and the request identifies an active
///   member (verified signature, active membership).
fn members_authorized(
    state: &AppState,
    q: &MembersFeedQuery,
    headers: &axum::http::HeaderMap,
) -> bool {
    let token_ok = match (state.members_token.as_str(), q.token.as_deref()) {
        (expected, Some(supplied)) if !expected.is_empty() => supplied == expected,
        _ => false,
    };
    if token_ok {
        return true;
    }
    if let Some(membership) = state.services.membership() {
        return membership.is_member(headers);
    }
    false
}

/// Compose the RSS channel. `site_name` is the operator-configured
/// deployment identity ([site] name in ccc.toml) — it brands the channel
/// description and iTunes author. Split from `build_feed_response` so the
/// channel content is unit-testable without an axum body.
fn build_feed_channel(
    title: &str,
    site_name: &str,
    episodes: &[ccc_database::PodcastEpisode],
) -> rss::Channel {
    let mut items = Vec::with_capacity(episodes.len());
    for ep in episodes {
        let pub_ts = ep.published_at.unwrap_or(ep.updated_at);
        let download_url = format!("/podcasts/download/{}", ep.id);
        let item = ItemBuilder::default()
            .title(Some(ep.title.clone()))
            .description(Some(ep.description.clone()))
            .link(Some(download_url.clone()))
            .pub_date(Some(rfc2822_utc(pub_ts)))
            .guid(Some(
                GuidBuilder::default()
                    .value(format!("urn:ccc:podcast:{}", ep.id))
                    .permalink(false)
                    .build(),
            ))
            .enclosure(Some(
                EnclosureBuilder::default()
                    .url(download_url)
                    .length(ep.size_bytes.to_string())
                    .mime_type(ep.mime_type.clone())
                    .build(),
            ))
            .itunes_ext(Some(
                ITunesItemExtensionBuilder::default()
                    .duration(if ep.duration_seconds > 0 {
                        Some(format_duration(ep.duration_seconds))
                    } else {
                        None
                    })
                    .build(),
            ))
            .build();
        items.push(item);
    }

    ChannelBuilder::default()
        .title(title)
        .link("/podcasts/feed.xml")
        .description(format!("Episodes published by {}.", site_name))
        .itunes_ext(Some(
            ITunesChannelExtensionBuilder::default()
                .author(Some(site_name.to_string()))
                .build(),
        ))
        .items(items)
        .build()
}

fn build_feed_response(
    title: &str,
    site_name: &str,
    episodes: &[ccc_database::PodcastEpisode],
) -> Response {
    let xml = build_feed_channel(title, site_name, episodes).to_string();
    (
        [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

#[derive(Template)]
#[template(path = "public_show.html")]
struct PublicShowPage {
    site_name: String,
    id: u64,
    title: String,
    description: String,
    duration: String,
    published_human: String,
}

async fn public_show(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let ep = match state.db.get_podcast_episode_by_slug(&slug) {
        Ok(Some(e)) if e.status == EpisodeStatus::Published => e,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicShowPage {
        site_name: state.site_name.as_ref().clone(),
        id: ep.id,
        title: ep.title,
        description: ep.description,
        duration: format_clock(ep.duration_seconds),
        published_human: format_unix(ep.published_at.unwrap_or(ep.updated_at)),
    })
}

async fn public_download(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<MembersFeedQuery>,
    Path(id): Path<u64>,
    headers: axum::http::HeaderMap,
) -> Response {
    let ep = match state.db.get_podcast_episode(id) {
        Ok(Some(e)) if e.status == EpisodeStatus::Published => e,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if ep.members_only && !members_authorized(&state, &q, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Hand the file off to tower_http::ServeFile, which handles
    // `Range: bytes=...` requests (returning 206 Partial Content with
    // Content-Range), If-Range revalidation, and HEAD correctly. This
    // matters for episodes that can be hundreds of MB: without Range
    // support a mobile client buffering over a flaky connection has to
    // pull the whole file before playback starts.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut req = axum::http::Request::builder()
        .method(axum::http::Method::GET)
        .header(header::CONTENT_TYPE, ep.mime_type.clone());
    if let Some(r) = range {
        req = req.header(header::RANGE, r);
    }
    let req = req.body(Body::empty()).unwrap();
    match ServeFile::new(&ep.audio_path).oneshot(req).await {
        Ok(resp) => resp.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage<'a> {
    episodes: Vec<EpisodeRow>,
    status: Option<&'a str>,
    members_token: String,
}

struct EpisodeRow {
    id: u64,
    title: String,
    slug: String,
    status_str: &'static str,
    size_human: String,
    updated_human: String,
}

async fn admin_list(State(state): State<AppState>) -> Response {
    let raw = match state.db.all_podcast_episodes() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Vec<EpisodeRow> = raw
        .into_iter()
        .map(|e| EpisodeRow {
            id: e.id,
            title: e.title,
            slug: e.slug,
            status_str: e.status.as_str(),
            size_human: human_bytes_fixed(e.size_bytes),
            updated_human: format_unix(e.updated_at),
        })
        .collect();
    render(AdminListPage {
        episodes: rows,
        status: None,
        members_token: state.members_token.as_ref().clone(),
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
    let mut queue_transcription = false;
    let mut audio_filename: Option<String> = None;
    let mut audio_path_on_disk: Option<PathBuf> = None;
    let mut audio_mime: Option<String> = None;
    let mut audio_size: u64 = 0;
    let mut members_only = false;
    let now = unix_now();

    if let Err(e) = std::fs::create_dir_all(state.audio_dir.as_ref()) {
        return new_form_error(&format!("Could not prepare audio directory: {}", e));
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
            "queue_transcription" => queue_transcription = true,
            "members_only" => members_only = true,
            "audio" => {
                let original = field.file_name().unwrap_or("upload.bin").to_string();
                let mime = field
                    .content_type()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "audio/mpeg".into());
                // Buffer the whole field in memory, then write to disk in one go.
                // Bounded by the route-level DefaultBodyLimit (MAX_UPLOAD_BYTES).
                let bytes = match field.bytes().await {
                    Ok(b) => b,
                    Err(e) => return new_form_error(&format!("Upload read error: {}", e)),
                };
                let safe_filename = format!("{}-{}", now, sanitize_filename_ascii(&original, "audio.bin"));
                let mut full = state.audio_dir.as_ref().clone();
                full.push(&safe_filename);
                let mut file = match tokio::fs::File::create(&full).await {
                    Ok(f) => f,
                    Err(e) => return new_form_error(&format!("Could not write audio file: {}", e)),
                };
                if let Err(e) = file.write_all(&bytes).await {
                    return new_form_error(&format!("Disk write error: {}", e));
                }
                let _ = file.flush().await;
                audio_filename = Some(original);
                audio_path_on_disk = Some(full);
                audio_mime = Some(mime);
                audio_size = bytes.len() as u64;
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
    let audio_path = match audio_path_on_disk {
        Some(p) => p,
        None => return new_form_error("Audio file is required."),
    };
    if audio_size == 0 {
        let _ = std::fs::remove_file(&audio_path);
        return new_form_error("The audio file was empty.");
    }

    let mut episode = PodcastEpisode {
        id: match state.db.next_id("podcast_episode") {
            Ok(id) => id,
            Err(e) => return new_form_error(&format!("Could not allocate episode id: {e}")),
        },
        slug,
        title,
        description,
        audio_path: audio_path.to_string_lossy().to_string(),
        mime_type: audio_mime.unwrap_or_else(|| "audio/mpeg".into()),
        size_bytes: audio_size,
        duration_seconds: 0,
        status: EpisodeStatus::Draft,
        created_at: now,
        updated_at: now,
        published_at: None,
        transcript_job_id: None,
        members_only,
    };

    // Optional Whisper integration: queue a transcription job that points
    // at the just-saved file. Whisper plugin's worker will pick it up.
    if queue_transcription {
        let job = TranscriptionJob {
            id: match state.db.next_id("transcription_job") {
                Ok(id) => id,
                Err(e) => return new_form_error(&format!("Could not allocate job id: {e}")),
            },
            audio_path: episode.audio_path.clone(),
            status: TranscriptionStatus::Queued,
            language: "auto".into(),
            model: "base".into(),
            transcript: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        if state.db.upsert_transcription_job(&job).is_ok() {
            episode.transcript_job_id = Some(job.id);
        }
    }

    if let Err(e) = state.db.upsert_podcast_episode(&episode) {
        let _ = std::fs::remove_file(&episode.audio_path);
        return new_form_error(&format!("Could not save episode: {}", e));
    }

    let _ = audio_filename; // kept for future "original filename" surfacing
    Redirect::to("/admin/podcasts").into_response()
}

#[derive(Template)]
#[template(path = "admin_edit.html")]
struct AdminEditPage<'a> {
    id: u64,
    title: String,
    slug: String,
    description: String,
    status_str: &'static str,
    audio_path: String,
    mime_type: String,
    size_human: String,
    created_human: String,
    updated_human: String,
    transcript_job_id: Option<u64>,
    members_only: bool,
    error: Option<&'a str>,
}

async fn admin_edit_form(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let ep = match state.db.get_podcast_episode(id) {
        Ok(Some(e)) => e,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(AdminEditPage {
        id: ep.id,
        title: ep.title,
        slug: ep.slug,
        description: ep.description,
        status_str: ep.status.as_str(),
        audio_path: ep.audio_path,
        mime_type: ep.mime_type,
        size_human: human_bytes_fixed(ep.size_bytes),
        created_human: format_unix(ep.created_at),
        updated_human: format_unix(ep.updated_at),
        transcript_job_id: ep.transcript_job_id,
        members_only: ep.members_only,
        error: None,
    })
}

#[derive(Deserialize)]
struct EditForm {
    title: String,
    slug: String,
    description: String,
    status: String,
    #[serde(default)]
    members_only: Option<String>,
}

async fn admin_update(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<EditForm>,
) -> Response {
    let mut ep = match state.db.get_podcast_episode(id) {
        Ok(Some(e)) => e,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = unix_now();
    let new_status = EpisodeStatus::from_form(&form.status);
    let was_published = ep.status == EpisodeStatus::Published;
    ep.title = form.title.trim().to_string();
    ep.slug = form.slug.trim().to_string();
    ep.description = form.description;
    ep.status = new_status;
    ep.members_only = form.members_only.is_some();
    ep.updated_at = now;
    let just_published = new_status == EpisodeStatus::Published && !was_published;
    if just_published {
        ep.published_at = Some(now);
    } else if new_status == EpisodeStatus::Draft {
        ep.published_at = None;
    }
    if let Err(e) = state.db.upsert_podcast_episode(&ep) {
        return render(AdminEditPage {
            id: ep.id,
            title: ep.title,
            slug: ep.slug,
            description: ep.description,
            status_str: ep.status.as_str(),
            audio_path: ep.audio_path,
            mime_type: ep.mime_type,
            size_human: human_bytes_fixed(ep.size_bytes),
            created_human: format_unix(ep.created_at),
            updated_human: format_unix(ep.updated_at),
            transcript_job_id: ep.transcript_job_id,
            members_only: ep.members_only,
            error: Some(&format!("Could not save: {}", e)),
        });
    }
    if just_published {
        state
            .events
            .publish(Event::PodcastEpisodePublished { id: ep.id });
    }
    Redirect::to("/admin/podcasts").into_response()
}

async fn admin_delete(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    if let Ok(Some(ep)) = state.db.get_podcast_episode(id) {
        let _ = std::fs::remove_file(&ep.audio_path);
    }
    let _ = state.db.delete_podcast_episode(id);
    Redirect::to("/admin/podcasts").into_response()
}

// ============================================================================
// Helpers
// ============================================================================
// The generic ones (render, human_bytes_fixed, sanitize_filename_ascii,
// format_clock, rfc2822_utc) live in ccc-plugin-utils; only
// podcast-specific formatting stays here.

fn new_form_error(msg: &str) -> Response {
    render(AdminNewPage {
        error: Some(msg),
        max_mb: MAX_UPLOAD_BYTES / (1024 * 1024),
    })
}

fn format_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2822_utc_format() {
        // Consumer-side regression guard: the RSS `pubDate` produced by
        // build_feed_response must stay byte-identical after the helper
        // moved to ccc-plugin-utils. (The full weekday/month coverage
        // lives next to the helper in that crate.)
        // 2024-01-15 12:34:56 UTC == 1705322096
        assert_eq!(
            rfc2822_utc(1_705_322_096),
            "Mon, 15 Jan 2024 12:34:56 +0000"
        );
        // Epoch
        assert_eq!(rfc2822_utc(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }

    #[test]
    fn feed_channel_carries_the_configured_site_name() {
        let ep = PodcastEpisode {
            id: 7,
            slug: "hello".into(),
            title: "Hello World".into(),
            description: "First episode".into(),
            audio_path: "/tmp/hello.mp3".into(),
            mime_type: "audio/mpeg".into(),
            size_bytes: 1234,
            duration_seconds: 61,
            status: EpisodeStatus::Published,
            created_at: 1_705_322_096,
            updated_at: 1_705_322_096,
            published_at: Some(1_705_322_096),
            transcript_job_id: None,
            members_only: false,
        };
        let ch = build_feed_channel(
            "Radio Free Harbor Podcast",
            "Radio Free Harbor",
            std::slice::from_ref(&ep),
        );
        assert_eq!(ch.title(), "Radio Free Harbor Podcast");
        assert_eq!(ch.description(), "Episodes published by Radio Free Harbor.");
        assert_eq!(
            ch.itunes_ext().and_then(|e| e.author()),
            Some("Radio Free Harbor")
        );
        let xml = ch.to_string();
        assert!(xml.contains("Hello World"), "item lost: {xml}");
        assert!(
            !xml.contains("Nuntius"),
            "platform name leaked into the public feed: {xml}"
        );
    }
}
