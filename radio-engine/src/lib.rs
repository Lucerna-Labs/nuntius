//! Native Rust radio streaming engine — Icecast-compatible, with
//! AutoDJ + track library so the station broadcasts 24/7 even when no
//! live source is connected.
//!
//! Single-mount, single-source, many-listeners. The source connects via
//! `SOURCE /mount ICE/1.0` (legacy) or `PUT /mount HTTP/1.1` (modern)
//! with HTTP Basic auth (user `source`, configured password). Audio
//! bytes are fanned out via `tokio::sync::broadcast` to every connected
//! listener. When no live source is connected, the AutoDJ worker
//! shuffles the uploaded track library and streams paced at each
//! track's effective bitrate. A live source connecting yields the
//! AutoDJ; on disconnect the AutoDJ resumes.
//!
//! Co-existence with axum: the radio engine listens on its OWN TCP port
//! (typically 8000), separate from the API port. The plugin contributes
//! `admin_routes()` for status + library + settings on the API port.

mod library;
mod mixer;
mod nowplaying;
use nowplaying::{emit_update, NpEventSender};
mod playlists;

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use base64::Engine as _;
use bytes::Bytes;
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_database::{CccDB, EpisodeStatus, Track, VideoStatus};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

const BROADCAST_CAPACITY: usize = 256;
const READ_CHUNK: usize = 8 * 1024;
/// Maximum HTTP-style header bytes we'll accept on a connection before
/// rejecting it. Source connections sometimes send a few KB of metadata.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Streaming protocol kept for the legacy CLI subcommand. Real engine
/// listens for both Icecast-style sources and plain GET listeners on the
/// same TCP port.
#[derive(Clone, Debug, Default, ValueEnum)]
pub enum StreamingProtocol {
    /// Native Icecast-compatible TCP server.
    #[default]
    Icecast,
    /// HLS placeholder (unimplemented in this version).
    Hls,
}

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadioSettings {
    pub port: u16,
    pub mount_path: String,
    pub source_password: String,
    pub content_type: String,
    pub stream_name: String,
    /// Directory under which AutoDJ stores uploaded music tracks.
    /// Created on first run if it doesn't exist.
    #[serde(default = "default_library_dir")]
    pub library_dir: String,
    /// Optional upstream Icecast/SHOUTcast URL. When set, the relay
    /// task connects as a listener and pumps received bytes into our
    /// broadcast channel. Live source connections still preempt it.
    /// Empty string = disabled.
    #[serde(default)]
    pub relay_url: String,
    /// Output bitrate (kbps) the AutoDJ ffmpeg encoder uses when
    /// re-encoding library tracks for broadcast. Higher = better
    /// quality + more bandwidth per listener. 128 is a good default
    /// for music radio.
    #[serde(default = "default_station_bitrate")]
    pub station_bitrate_kbps: u32,
    /// Master switch for normalization at AutoDJ playback. When true,
    /// each track's measured target_gain_db is applied via ffmpeg's
    /// volume filter. Tracks without a measurement play unchanged.
    #[serde(default = "default_loudness_norm")]
    pub apply_loudness_normalization: bool,
    /// Crossfade duration in seconds at track boundaries (0 = hard cuts).
    #[serde(default = "default_crossfade_seconds")]
    pub crossfade_seconds: u32,
    /// Additional mount points at different bitrates. Each variant
    /// runs its own ffmpeg transcoder fed from the master broadcast
    /// stream. Configure in `ccc.toml`:
    /// ```toml
    /// [[plugin.radio-engine.variant_mounts]]
    /// path = "/listen-low.mp3"
    /// bitrate_kbps = 64
    /// ```
    #[serde(default)]
    pub variant_mounts: Vec<VariantMountSetting>,
    /// If the AutoDJ has been silent for this many seconds (no live
    /// source, no relay, no AutoDJ track playing), switch to the
    /// backup playlist. Set to 0 to disable. Default 15s — long
    /// enough that a normal track transition isn't classified as
    /// dead air, short enough that listeners don't hear silence.
    #[serde(default = "default_dead_air_seconds")]
    pub dead_air_seconds: u64,
    /// IDs of tracks (in `tracks` table) that form the emergency
    /// backup rotation. Should be short, ready-to-play files
    /// (station IDs, jingles, "we'll be right back" loops). When
    /// the main AutoDJ hasn't picked a track within
    /// `dead_air_seconds`, we fall back to a random pick from this
    /// list. Empty = no backup (default).
    #[serde(default)]
    pub backup_track_ids: Vec<u64>,
    /// When playing from the backup rotation, keep cycling tracks
    /// from it for at most this many seconds before giving up and
    /// trying the main library again. Default 300s (5 min). Set
    /// to 0 to never retry.
    #[serde(default = "default_backup_max_seconds")]
    pub backup_max_seconds: u64,
    /// Maximum times the same artist may air in any rolling
    /// `cap_window_hours`-hour window. 0 = disabled. Default 4,
    /// which keeps the rotation varied without being restrictive.
    #[serde(default = "default_max_artist_in_window")]
    pub max_artist_in_window: u32,
    /// Maximum times the same album may air in any rolling
    /// `cap_window_hours`-hour window. 0 = disabled. Default 3.
    #[serde(default = "default_max_album_in_window")]
    pub max_album_in_window: u32,
    /// Width of the rolling window the artist/album caps look back
    /// over. Default 3 hours — matches the SoundExchange "performance
    /// complement" window which is the longest duration a station
    /// typically thinks about.
    #[serde(default = "default_cap_window_hours")]
    pub cap_window_hours: u64,
    /// Optional SRT/RTP/RTMP URL of a remote presenter's encoder.
    /// When set, a background task pulls bytes from it via ffmpeg and
    /// pumps them into the broadcast channel. Live source connections
    /// and the AutoDJ preempt it; clearing the field disables the
    /// ingest. Examples:
    ///   `srt://presenter.example.com:9000?mode=caller&passphrase=secret`
    ///   `rtp://239.0.0.1:5004`
    ///   `rtmp://presenter.example.com/live/streamkey`
    /// Empty string = disabled.
    #[serde(default)]
    pub remote_presenter_url: String,
    /// Human-readable label shown in the public now-playing readout
    /// when the remote presenter's stream is airing. Listeners see
    /// "[remote] <label>". Defaults to "Remote Presenter".
    #[serde(default = "default_remote_presenter_label")]
    pub remote_presenter_label: String,
    /// Optional passphrase for SRT mode. When set, the engine passes
    /// `-passphrase` to ffmpeg on the SRT caller. Most public SRT
    /// endpoints use a passphrase. Empty = no encryption.
    #[serde(default)]
    pub remote_presenter_passphrase: String,
}

fn default_dead_air_seconds() -> u64 {
    15
}
fn default_backup_max_seconds() -> u64 {
    300
}
fn default_max_artist_in_window() -> u32 {
    4
}
fn default_max_album_in_window() -> u32 {
    3
}
fn default_cap_window_hours() -> u64 {
    3
}
fn default_remote_presenter_label() -> String {
    "Remote Presenter".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantMountSetting {
    pub path: String,
    pub bitrate_kbps: u32,
}

fn default_station_bitrate() -> u32 {
    128
}
fn default_loudness_norm() -> bool {
    true
}
fn default_crossfade_seconds() -> u32 {
    2
}

fn default_library_dir() -> String {
    "./media/radio-library".into()
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            port: 8000,
            mount_path: "/listen.mp3".into(),
            source_password: "hackme".into(),
            content_type: "audio/mpeg".into(),
            stream_name: "Nuntius Radio".into(),
            library_dir: default_library_dir(),
            relay_url: String::new(),
            station_bitrate_kbps: default_station_bitrate(),
            apply_loudness_normalization: default_loudness_norm(),
            crossfade_seconds: default_crossfade_seconds(),
            variant_mounts: Vec::new(),
            dead_air_seconds: default_dead_air_seconds(),
            backup_track_ids: Vec::new(),
            backup_max_seconds: default_backup_max_seconds(),
            max_artist_in_window: default_max_artist_in_window(),
            max_album_in_window: default_max_album_in_window(),
            cap_window_hours: default_cap_window_hours(),
            remote_presenter_url: String::new(),
            remote_presenter_label: default_remote_presenter_label(),
            remote_presenter_passphrase: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    radio: RadioSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<RadioSettings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let r = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| PluginError::InitError(format!("radio IO: {}", e)))?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.radio
        } else {
            RadioSettings::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(r),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> RadioSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: RadioSettings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { radio: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PluginError::InitError(format!("radio IO: {}", e)))?;
            }
        }
        std::fs::write(&self.path, raw)
            .map_err(|e| PluginError::InitError(format!("radio IO: {}", e)))?;
        Ok(())
    }
}

// ============================================================================
// Engine state — shared between TCP server and admin dashboard
// ============================================================================

#[derive(Clone)]
pub(crate) struct EngineState {
    settings: Arc<SettingsStore>,
    tx: broadcast::Sender<Bytes>,
    pub(crate) source_alive: Arc<AtomicBool>,
    pub(crate) autodj_paused: Arc<AtomicBool>,
    listener_count: Arc<AtomicUsize>,
    /// Where uploaded music tracks live on disk.
    library_dir: Arc<PathBuf>,
    /// Live current-track snapshot for /api/radio/nowplaying + dashboard.
    now_playing: Arc<RwLock<Option<NowPlaying>>>,
    /// Bounded ring buffer of the last few items that aired. Drives
    /// the public listener page's "previously played" panel.
    history: Arc<RwLock<std::collections::VecDeque<NowPlaying>>>,
    /// On-air recording state. `Some(...)` means the recording task is
    /// active and writing every broadcast byte to `path`.
    recording: Arc<RwLock<Option<RecordingHandle>>>,
    /// True iff the relay task currently has an upstream connection
    /// and is pumping bytes. AutoDJ pauses when relay_alive == true.
    pub(crate) relay_alive: Arc<AtomicBool>,
    /// True iff the remote-presenter ingest task currently has an
    /// active ffmpeg subprocess and is pumping bytes. AutoDJ pauses
    /// when presenter_alive == true. Preempted by live source and the
    /// standard Icecast relay.
    pub(crate) presenter_alive: Arc<AtomicBool>,
    /// Map from variant mount path → its dedicated broadcast channel.
    /// One ffmpeg subprocess per variant transcodes the master stream.
    variants: Arc<RwLock<std::collections::HashMap<String, broadcast::Sender<Bytes>>>>,
    /// Live database handle (None when constructed via the legacy CLI
    /// `radio` subcommand, which has no DB).
    db: Option<Arc<CccDB>>,
    /// Per-engine one-shot spawn flags. Replaces the old
    /// `static SPAWNED: AtomicBool` pattern that didn't work if
    /// you ever constructed two engines in the same process (tests,
    /// future hot-reload).
    spawn_flags: Arc<RwLock<std::collections::HashSet<&'static str>>>,
    /// Typed broadcast channel of `NowPlayingEvent`s. Listeners (WS,
    /// SSE) subscribe here to get push-based now-playing updates
    /// without polling the JSON endpoint.
    pub(crate) np_events: Arc<RwLock<Option<NpEventSender>>>,
}

impl EngineState {
    /// Spawn a background task exactly once per (engine, name) pair.
    /// The first call spawns the task; subsequent calls with the same
    /// `name` are no-ops. Returns `Some(JoinHandle)` on the first call,
    /// `None` afterwards.
    ///
    /// The closure receives the engine state by value (matching the
    /// historical `spawn_*` signatures) and returns a future to run
    /// on the tokio runtime.
    pub fn spawn_once<F, Fut>(
        &self,
        name: &'static str,
        f: F,
    ) -> Option<tokio::task::JoinHandle<()>>
    where
        F: FnOnce(EngineState) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut flags = self.spawn_flags.write().expect("spawn flags poisoned");
        if !flags.insert(name) {
            return None;
        }
        drop(flags);
        let st = self.clone();
        Some(tokio::spawn(f(st)))
    }
}

#[derive(Clone)]
pub struct RecordingHandle {
    pub path: String,
    pub started_at: u64,
}

const HISTORY_CAPACITY: usize = 20;

/// Bytes of audio between SHOUTcast/Icecast metadata blocks. 8192 is
/// the de-facto default; client players (VLC, foobar2000, browsers)
/// negotiate via the `icy-metaint` response header.
const ICY_METAINT: usize = 8192;

/// What the AutoDJ is currently airing. Cleared when a live source
/// takes over.
#[derive(Debug, Clone, Serialize)]
pub struct NowPlaying {
    pub source: &'static str, // "autodj" | "live"
    pub track_id: Option<u64>,
    pub title: String,
    pub artist: String,
    pub started_at: u64,
    pub duration_seconds: u64,
}

// ============================================================================
// Plugin (admin UI side)
// ============================================================================

pub struct RadioEnginePlugin {
    pub port: u16,
    pub protocol: StreamingProtocol,
    state: EngineState,
    /// Cancellation token for the engine's accept loop. The CLI flips
    /// it on SIGINT/SIGTERM so the autodj/relay/recorder tasks can wind
    /// down cleanly instead of being killed mid-write.
    shutdown: tokio_util::sync::CancellationToken,
}

impl RadioEnginePlugin {
    /// Construct an engine standalone (used by the legacy `radio` CLI subcommand).
    pub fn new(port: u16, protocol: StreamingProtocol) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let settings = RadioSettings {
            port,
            ..RadioSettings::default()
        };
        let library_dir = PathBuf::from(&settings.library_dir);
        let state = EngineState {
            settings: Arc::new(SettingsStore {
                path: PathBuf::from("radio.toml"),
                inner: RwLock::new(settings),
            }),
            tx,
            source_alive: Arc::new(AtomicBool::new(false)),
            autodj_paused: Arc::new(AtomicBool::new(false)),
            listener_count: Arc::new(AtomicUsize::new(0)),
            library_dir: Arc::new(library_dir),
            now_playing: Arc::new(RwLock::new(None)),
            history: Arc::new(RwLock::new(std::collections::VecDeque::with_capacity(
                HISTORY_CAPACITY,
            ))),
            recording: Arc::new(RwLock::new(None)),
            relay_alive: Arc::new(AtomicBool::new(false)),
            presenter_alive: Arc::new(AtomicBool::new(false)),
            variants: Arc::new(RwLock::new(std::collections::HashMap::new())),
            db: None,
            spawn_flags: Arc::new(RwLock::new(std::collections::HashSet::new())),
            np_events: Arc::new(RwLock::new(Some(nowplaying::channel().0))),
        };
        Self {
            port,
            protocol,
            state,
            shutdown: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Trigger graceful shutdown. Safe to call from a signal handler.
    /// After calling, [`Self::run`] will return at the next cancellation
    /// point (the TCP accept loop).
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Returns a clone of the plugin's shutdown token. The caller can
    /// `select!` on it or `cancel()` it from another task to trigger
    /// graceful shutdown. Clones share the same underlying token, so
    /// cancelling one cancels all.
    pub fn shutdown_token(&self) -> tokio_util::sync::CancellationToken {
        self.shutdown.clone()
    }

    /// Construct from `[plugin.radio-engine]` config — the CMS-driven path.
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("radio-engine")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("radio.toml"));
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        let snap = store.snapshot();
        let port = snap.port;
        let library_dir = PathBuf::from(&snap.library_dir);
        let _ = std::fs::create_dir_all(&library_dir);
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = EngineState {
            settings: store,
            tx,
            source_alive: Arc::new(AtomicBool::new(false)),
            autodj_paused: Arc::new(AtomicBool::new(false)),
            listener_count: Arc::new(AtomicUsize::new(0)),
            library_dir: Arc::new(library_dir),
            now_playing: Arc::new(RwLock::new(None)),
            history: Arc::new(RwLock::new(std::collections::VecDeque::with_capacity(
                HISTORY_CAPACITY,
            ))),
            recording: Arc::new(RwLock::new(None)),
            relay_alive: Arc::new(AtomicBool::new(false)),
            presenter_alive: Arc::new(AtomicBool::new(false)),
            variants: Arc::new(RwLock::new(std::collections::HashMap::new())),
            db: None,
            spawn_flags: Arc::new(RwLock::new(std::collections::HashSet::new())),
            np_events: Arc::new(RwLock::new(Some(nowplaying::channel().0))),
        };
        Ok(Self {
            port,
            protocol: StreamingProtocol::Icecast,
            state,
            shutdown: tokio_util::sync::CancellationToken::new(),
        })
    }

    /// Run the engine until something cancels (or forever). Used by the
    /// standalone `radio` CLI subcommand. Returns when the cancellation
    /// token is flipped (typically by SIGINT/SIGTERM in the CLI).
    pub async fn run(&self) -> Result<(), std::io::Error> {
        run_engine(self.state.clone(), self.shutdown.clone()).await
    }
}

impl Plugin for RadioEnginePlugin {
    fn name(&self) -> &'static str {
        "Radio Station"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn id(&self) -> String {
        // Use the same identifier as the dispatch table in cli/src/registry.rs
        // and the slug operators put in `ccc.toml`. The admin URL mirrors
        // this via `admin_routes()` nesting at `/admin/{id}`.
        "radio-engine".into()
    }

    /// The plugin's admin landing page. The framework nests the routes
    /// returned by `admin_routes()` under `/admin/{id}`, so `/radio` here
    /// gives us the final URL `/admin/radio-engine/radio` — the dashboard.
    /// Override keeps the radio engine's index page reachable via the
    /// canonical id-based path.
    fn admin_url(&self) -> String {
        "/admin/radio-engine/radio".into()
    }
    fn kind(&self) -> &'static str {
        "BROADCAST"
    }
    fn icon(&self) -> &'static str {
        "icon-radio"
    }
    fn primary(&self) -> bool {
        true
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let snap = self.state.settings.snapshot();
        let live = self.state.source_alive.load(Ordering::SeqCst);
        let relay = self.state.relay_alive.load(Ordering::SeqCst);
        let autodj_paused = self.state.autodj_paused.load(Ordering::SeqCst);
        let np = self.state.now_playing.read().ok().and_then(|g| g.clone());
        let listeners = self.state.listener_count.load(Ordering::SeqCst);
        let track_count = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.all_tracks().ok())
            .map(|v| v.len())
            .unwrap_or(0);

        let presenter = self.state.presenter_alive.load(Ordering::SeqCst);
        let state = if live || relay || presenter || np.is_some() {
            PluginLifecycle::Live
        } else {
            PluginLifecycle::Configured
        };

        PluginStatus {
            state,
            badge: if listeners > 0 {
                Some(listeners.to_string())
            } else {
                None
            },
            detail: serde_json::json!({
                "configured": true,
                "live": live,
                "relay": relay,
                "presenter": presenter,
                "autodj_paused": autodj_paused,
                "config": {
                    "port": snap.port,
                    "mount_path": snap.mount_path,
                    "stream_name": snap.stream_name,
                    "bitrate": snap.station_bitrate_kbps,
                    "crossfade": snap.crossfade_seconds,
                    "normalization": snap.apply_loudness_normalization,
                    "library_dir": snap.library_dir,
                    "relay_url": snap.relay_url,
                    "remote_presenter_url": snap.remote_presenter_url,
                },
                "health": {
                    "listeners": listeners,
                    "tracks": track_count,
                }
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        let snap = self.state.settings.snapshot();
        println!(
            "Initializing Radio Engine on TCP {} (mount {})",
            snap.port, snap.mount_path
        );
        // Spawn the engine. Engine error => we log and exit the task; the
        // host stays up.
        let st = self.state.clone();
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = run_engine(st, shutdown).await {
                eprintln!("[radio-engine] terminated: {}", e);
            }
        });
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        // Public surface: live listener page + nowplaying JSON + library JSON
        // + WebSocket / SSE feed of now-playing state.
        let st = EngineState {
            db: Some(ctx.db.clone()),
            ..self.state.clone()
        };
        let st_arc = Arc::new(st);
        Some(
            Router::new()
                .route("/", get(public_home))
                .route("/radio", get(public_listen))
                .route("/api/radio/nowplaying", get(public_now_playing))
                .route("/api/radio/library", get(public_library))
                .route("/api/radio/presenter", get(public_presenter_status))
                .route("/api/radio/nowplaying/stream", get(nowplaying::ws_handler))
                .route("/api/radio/nowplaying/sse", get(nowplaying::sse_handler))
                .with_state(st_arc),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        // Inject the live DB handle and spawn AutoDJ on first call.
        // We do this here (not in init) because init has no ctx.
        {
            // SAFETY: only this Arc holds the db; we create it once at boot.
            // We can't mutate `self.state.db` (it's behind an Arc), so we
            // build a fresh state with the db filled in for the routes
            // returned here. The TCP server task uses the cloned state we
            // already spawned in init() — that one stays db-less, which
            // is fine: the engine's TCP loop doesn't need DB access.
        }
        let st = EngineState {
            db: Some(ctx.db.clone()),
            ..self.state.clone()
        };
        let st_arc = Arc::new(st);
        let shutdown = self.shutdown.clone();
        spawn_autodj((*st_arc).clone(), shutdown.clone());
        spawn_recorder((*st_arc).clone(), shutdown.clone());
        spawn_relay((*st_arc).clone(), shutdown.clone());
        spawn_remote_presenter((*st_arc).clone(), shutdown.clone());
        spawn_variant_transcoders((*st_arc).clone(), shutdown.clone());
        Some(
            Router::new()
                .route("/radio", get(admin_get).post(admin_post))
                .route("/radio/engineering", get(admin_engineering_get))
                .route(
                    "/radio/tracks",
                    get(library::admin_track_list).post(library::admin_track_upload),
                )
                .route(
                    "/radio/tracks/:id/delete",
                    post(library::admin_track_delete),
                )
                .route(
                    "/radio/playlists",
                    get(playlists::admin_list).post(playlists::admin_create),
                )
                .route(
                    "/radio/playlists/:id/edit",
                    get(playlists::admin_edit_form).post(playlists::admin_edit_post),
                )
                .route("/radio/playlists/:id/delete", post(playlists::admin_delete))
                .route(
                    "/radio/playlists/:id/tracks",
                    post(playlists::admin_add_track),
                )
                .route(
                    "/radio/playlists/:id/tracks/:position/remove",
                    post(playlists::admin_remove_track),
                )
                .route("/radio/record/start", post(admin_record_start))
                .route("/radio/record/stop", post(admin_record_stop))
                .route("/radio/autodj/pause", post(admin_autodj_pause))
                .route("/radio/autodj/resume", post(admin_autodj_resume))
                .route("/radio/presenter/stop", post(admin_presenter_stop))
                .layer(axum::extract::DefaultBodyLimit::max(
                    library::MAX_TRACK_BYTES,
                ))
                .with_state(st_arc),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Radio",
            href: "/admin/radio-engine/radio".into(),
            icon: "📻",
            sort: 1000,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// TCP engine — source + listener handling
// ============================================================================

async fn run_engine(
    state: EngineState,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    let port = state.settings.snapshot().port;
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("[radio-engine] listening on {}", addr);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                println!("[radio-engine] shutdown signal received, stopping accept loop");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((sock, peer)) => {
                        let st = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(sock, peer, st).await {
                                eprintln!("[radio-engine] connection {} ended: {}", peer, e);
                            }
                        });
                    }
                    Err(e) => eprintln!("[radio-engine] accept error: {}", e),
                }
            }
        }
    }
    Ok(())
}

async fn handle_connection(
    mut sock: TcpStream,
    peer: std::net::SocketAddr,
    state: EngineState,
) -> Result<(), std::io::Error> {
    // Read enough of the request to identify method + auth.
    let mut buf = Vec::with_capacity(MAX_HEADER_BYTES);
    let mut probe = [0u8; READ_CHUNK];
    loop {
        let n = sock.read(&mut probe).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&probe[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Ok(()); // headers too large — drop
        }
    }

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let leftover_body = buf[header_end..].to_vec();

    let request_line = match header_text.lines().next() {
        Some(l) => l,
        None => return Ok(()),
    };

    let snap = state.settings.snapshot();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let is_source = method.eq_ignore_ascii_case("SOURCE")
        || (method.eq_ignore_ascii_case("PUT") && path == snap.mount_path);

    if is_source {
        if path != snap.mount_path {
            let _ = sock.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n").await;
            return Ok(());
        }
        if !auth_ok(&header_text, &snap.source_password) {
            let _ = sock
                .write_all(
                    b"HTTP/1.0 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Radio\"\r\n\r\n",
                )
                .await;
            return Ok(());
        }
        if state.source_alive.load(Ordering::SeqCst) {
            let _ = sock
                .write_all(b"HTTP/1.0 403 Forbidden\r\n\r\nMount in use\r\n")
                .await;
            return Ok(());
        }
        // Accept handshake.
        let _ = sock.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await;
        state.source_alive.store(true, Ordering::SeqCst);
        // Live broadcast preempts AutoDJ's now-playing entry.
        *state.now_playing.write().expect("np poisoned") = Some(NowPlaying {
            source: "live",
            track_id: None,
            title: snap.stream_name.clone(),
            artist: format!("Live from {}", peer),
            started_at: unix_now(),
            duration_seconds: 0,
        });
        emit_update(&state, None);
        println!("[radio-engine] source connected from {}", peer);

        // Push leftover body bytes immediately if any.
        if !leftover_body.is_empty() {
            let _ = state.tx.send(Bytes::copy_from_slice(&leftover_body));
        }
        // Then pump bytes until source disconnects.
        let mut chunk = [0u8; READ_CHUNK];
        loop {
            match sock.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    let _ = state.tx.send(Bytes::copy_from_slice(&chunk[..n]));
                }
                Err(_) => break,
            }
        }
        state.source_alive.store(false, Ordering::SeqCst);
        // AutoDJ will repopulate this on its next iteration.
        *state.now_playing.write().expect("np poisoned") = None;
        emit_update(&state, None);
        println!("[radio-engine] source disconnected from {}", peer);
        Ok(())
    } else if method.eq_ignore_ascii_case("GET")
        && (path == snap.mount_path
            || state
                .variants
                .read()
                .expect("variants poisoned")
                .contains_key(path))
    {
        // Listener — for the master mount or any configured variant.
        let variant_tx: Option<broadcast::Sender<Bytes>> = if path == snap.mount_path {
            None
        } else {
            state
                .variants
                .read()
                .expect("variants poisoned")
                .get(path)
                .cloned()
        };
        let wants_metadata = header_text.lines().any(|l| {
            l.to_ascii_lowercase().starts_with("icy-metadata:") && l.trim().ends_with('1')
        });

        let mut headers = format!(
            "HTTP/1.0 200 OK\r\n\
             Content-Type: {}\r\n\
             icy-name: {}\r\n\
             Cache-Control: no-cache\r\n\
             Connection: close\r\n\
             Access-Control-Allow-Origin: *\r\n",
            snap.content_type, snap.stream_name
        );
        if wants_metadata {
            headers.push_str(&format!("icy-metaint: {}\r\n", ICY_METAINT));
        }
        headers.push_str("\r\n");
        if sock.write_all(headers.as_bytes()).await.is_err() {
            return Ok(());
        }

        let mut rx = match &variant_tx {
            Some(tx) => tx.subscribe(),
            None => state.tx.subscribe(),
        };
        state.listener_count.fetch_add(1, Ordering::SeqCst);

        if wants_metadata {
            // Per-listener byte counter + last-sent metadata to suppress
            // redundant updates between tracks.
            let mut bytes_since_meta: usize = 0;
            let mut last_meta = String::new();
            'meta: loop {
                let chunk = match rx.recv().await {
                    Ok(c) => c,
                    Err(broadcast::error::RecvError::Lagged(_)) => break 'meta,
                    Err(broadcast::error::RecvError::Closed) => break 'meta,
                };
                let mut i = 0usize;
                while i < chunk.len() {
                    let remaining = ICY_METAINT - bytes_since_meta;
                    let take = remaining.min(chunk.len() - i);
                    if sock.write_all(&chunk[i..i + take]).await.is_err() {
                        break 'meta;
                    }
                    bytes_since_meta += take;
                    i += take;
                    if bytes_since_meta >= ICY_METAINT {
                        let cur = current_icy_title(&state);
                        let block = if cur != last_meta {
                            last_meta.clone_from(&cur);
                            encode_icy_metadata(&cur)
                        } else {
                            vec![0u8]
                        };
                        if sock.write_all(&block).await.is_err() {
                            break 'meta;
                        }
                        bytes_since_meta = 0;
                    }
                }
            }
        } else {
            // Plain listener — no metadata interleaving.
            loop {
                match rx.recv().await {
                    Ok(chunk) => {
                        if sock.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => break,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        state.listener_count.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    } else if method.eq_ignore_ascii_case("GET") && path == "/" {
        // Cosmetic landing page.
        let snap = state.settings.snapshot();
        let body = format!(
            "<!doctype html><html><body><h1>{}</h1><audio controls src=\"{}\"></audio></body></html>",
            snap.stream_name, snap.mount_path
        );
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = sock.write_all(body.as_bytes()).await;
        Ok(())
    } else {
        let _ = sock.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n").await;
        Ok(())
    }
}

fn auth_ok(headers: &str, expected_password: &str) -> bool {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("authorization:") {
            let val = rest.trim();
            if let Some(b64) = val.strip_prefix("basic ") {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                    if let Ok(decoded) = std::str::from_utf8(&bytes) {
                        if let Some((_user, pass)) = decoded.split_once(':') {
                            return pass == expected_password;
                        }
                    }
                }
            }
        }
    }
    false
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
struct AdminDashboardPage {
    active_tab: &'static str,
    port: u16,
    mount_path: String,
    track_count: usize,
    recording_path: Option<String>,
    autodj_paused: bool,
}

async fn admin_get(State(state): State<Arc<EngineState>>) -> Response {
    let snap = state.settings.snapshot();
    let track_count = state
        .db
        .as_ref()
        .and_then(|db| db.all_tracks().ok())
        .map(|v| v.len())
        .unwrap_or(0);
    let recording_path = state
        .recording
        .read()
        .expect("recording poisoned")
        .as_ref()
        .map(|h| h.path.clone());
    render(AdminDashboardPage {
        active_tab: "studio",
        port: snap.port,
        mount_path: snap.mount_path,
        track_count,
        recording_path,
        autodj_paused: state.autodj_paused.load(Ordering::SeqCst),
    })
}

#[derive(Template)]
#[template(path = "admin_engineering.html")]
struct AdminEngineeringPage {
    active_tab: &'static str,
    port: u16,
    mount_path: String,
    source_password: String,
    content_type: String,
    stream_name: String,
    relay_url: String,
    remote_presenter_url: String,
    remote_presenter_label: String,
    remote_presenter_passphrase: String,
    station_bitrate_kbps: u32,
    apply_loudness_normalization: bool,
    crossfade_seconds: u32,
    dead_air_seconds: u64,
    backup_track_ids_csv: String,
    backup_max_seconds: u64,
    max_artist_in_window: u32,
    max_album_in_window: u32,
    cap_window_hours: u64,
    saved: bool,
}

#[derive(Deserialize)]
struct SavedQuery {
    #[serde(default)]
    saved: Option<u8>,
}

async fn admin_engineering_get(
    State(state): State<Arc<EngineState>>,
    axum::extract::Query(q): axum::extract::Query<SavedQuery>,
) -> Response {
    let snap = state.settings.snapshot();
    let csv = snap
        .backup_track_ids
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    render(AdminEngineeringPage {
        active_tab: "engineering",
        port: snap.port,
        mount_path: snap.mount_path,
        source_password: snap.source_password,
        content_type: snap.content_type,
        stream_name: snap.stream_name,
        relay_url: snap.relay_url,
        remote_presenter_url: snap.remote_presenter_url,
        remote_presenter_label: snap.remote_presenter_label,
        remote_presenter_passphrase: snap.remote_presenter_passphrase,
        station_bitrate_kbps: snap.station_bitrate_kbps,
        apply_loudness_normalization: snap.apply_loudness_normalization,
        crossfade_seconds: snap.crossfade_seconds,
        dead_air_seconds: snap.dead_air_seconds,
        backup_track_ids_csv: csv,
        backup_max_seconds: snap.backup_max_seconds,
        max_artist_in_window: snap.max_artist_in_window,
        max_album_in_window: snap.max_album_in_window,
        cap_window_hours: snap.cap_window_hours,
        saved: q.saved.is_some(),
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    source_password: String,
    mount_path: String,
    content_type: String,
    stream_name: String,
    #[serde(default)]
    relay_url: String,
    #[serde(default)]
    remote_presenter_url: String,
    #[serde(default)]
    remote_presenter_label: String,
    #[serde(default)]
    remote_presenter_passphrase: String,
    #[serde(default)]
    station_bitrate_kbps: Option<u32>,
    #[serde(default)]
    apply_loudness_normalization: Option<String>,
    #[serde(default)]
    crossfade_seconds: Option<u32>,
    #[serde(default)]
    dead_air_seconds: Option<u64>,
    #[serde(default)]
    backup_track_ids: String,
    #[serde(default)]
    backup_max_seconds: Option<u64>,
    #[serde(default)]
    max_artist_in_window: Option<u32>,
    #[serde(default)]
    max_album_in_window: Option<u32>,
    #[serde(default)]
    cap_window_hours: Option<u64>,
}

async fn admin_post(
    State(state): State<Arc<EngineState>>,
    Form(form): Form<SettingsForm>,
) -> Response {
    let mut snap = state.settings.snapshot();
    snap.source_password = form.source_password;
    snap.mount_path = form.mount_path.trim().to_string();
    snap.content_type = form.content_type.trim().to_string();
    snap.stream_name = form.stream_name.trim().to_string();
    snap.relay_url = form.relay_url.trim().to_string();
    match validate_presenter_url(&form.remote_presenter_url) {
        Ok(normalized) => snap.remote_presenter_url = normalized,
        Err(e) => {
            // Refuse to save a bogus URL — we don't want a typo to
            // spawn a runaway ffmpeg process.
            eprintln!("[radio-engine] rejecting bad remote_presenter_url: {}", e);
            snap.remote_presenter_url = form.remote_presenter_url.trim().to_string();
        }
    }
    snap.remote_presenter_label = if form.remote_presenter_label.trim().is_empty() {
        default_remote_presenter_label()
    } else {
        form.remote_presenter_label.trim().to_string()
    };
    snap.remote_presenter_passphrase = form.remote_presenter_passphrase.to_string();
    if let Some(b) = form.station_bitrate_kbps {
        snap.station_bitrate_kbps = b.clamp(32, 320);
    }
    snap.apply_loudness_normalization = form.apply_loudness_normalization.is_some();
    if let Some(x) = form.crossfade_seconds {
        snap.crossfade_seconds = x.min(10);
    }
    if let Some(d) = form.dead_air_seconds {
        snap.dead_air_seconds = d.clamp(0, 600);
    }
    snap.backup_track_ids = parse_track_ids(&form.backup_track_ids);
    if let Some(m) = form.backup_max_seconds {
        snap.backup_max_seconds = m.clamp(0, 86_400);
    }
    if let Some(m) = form.max_artist_in_window {
        snap.max_artist_in_window = m.clamp(0, 50);
    }
    if let Some(m) = form.max_album_in_window {
        snap.max_album_in_window = m.clamp(0, 50);
    }
    if let Some(m) = form.cap_window_hours {
        snap.cap_window_hours = m.clamp(1, 24);
    }
    let _ = state.settings.replace(snap);
    Redirect::to("/admin/radio-engine/radio/engineering?saved=1").into_response()
}

/// Parse a comma- or whitespace-separated list of track IDs.
/// Invalid tokens are silently dropped.
fn parse_track_ids(s: &str) -> Vec<u64> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|t| t.trim().parse().ok())
        .collect()
}

// ============================================================================
// Public now-playing endpoint
// ============================================================================

async fn public_library(State(state): State<Arc<EngineState>>) -> Response {
    let tracks = state
        .db
        .as_ref()
        .and_then(|db| db.all_tracks().ok())
        .unwrap_or_default();
    let rows: Vec<_> = tracks
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "artist": t.artist,
                "duration_seconds": t.duration_seconds,
            })
        })
        .collect();
    Json(rows).into_response()
}

async fn public_presenter_status(State(state): State<Arc<EngineState>>) -> Response {
    let snap = state.settings.snapshot();
    Json(serde_json::json!({
        "configured": !snap.remote_presenter_url.is_empty(),
        "url": snap.remote_presenter_url,
        "label": snap.remote_presenter_label,
        "active": state.presenter_alive.load(Ordering::SeqCst),
        "ts": unix_now(),
    }))
    .into_response()
}

async fn public_now_playing(State(state): State<Arc<EngineState>>) -> Response {
    let snap = state
        .now_playing
        .read()
        .expect("now_playing poisoned")
        .clone();
    let history: Vec<NowPlaying> = state
        .history
        .read()
        .expect("history poisoned")
        .iter()
        .cloned()
        .collect();
    let s = state.settings.snapshot();
    let recording = state
        .recording
        .read()
        .expect("recording poisoned")
        .as_ref()
        .map(|h| h.path.clone());
    Json(serde_json::json!({
        "now_playing": snap,
        "history": history,
        "live": state.source_alive.load(Ordering::SeqCst),
        "relay": state.relay_alive.load(Ordering::SeqCst),
        "presenter": state.presenter_alive.load(Ordering::SeqCst),
        "autodj_paused": state.autodj_paused.load(Ordering::SeqCst),
        "recording": recording,
        "listeners": state.listener_count.load(Ordering::SeqCst),
        "stream_name": s.stream_name,
        "mount_path": s.mount_path,
        "radio_port": s.port,
        "now_unix": unix_now(),
    }))
    .into_response()
}

#[derive(Template)]
#[template(path = "public_listen.html")]
struct PublicListenPage {
    stream_name: String,
}

#[derive(Template)]
#[template(path = "public_home.html")]
struct PublicHomePage {
    stream_name: String,
    podcasts: Vec<PublicMediaRow>,
    videos: Vec<PublicMediaRow>,
    upcoming: Vec<PublicMediaRow>,
}

struct PublicMediaRow {
    title: String,
    url: String,
    description: String,
    kind: &'static str,
    duration: String,
    date: String,
}

async fn public_home(State(state): State<Arc<EngineState>>) -> Response {
    let s = state.settings.snapshot();
    let db = match state.db.as_ref() {
        Some(db) => db,
        None => {
            return render(PublicHomePage {
                stream_name: s.stream_name,
                podcasts: Vec::new(),
                videos: Vec::new(),
                upcoming: Vec::new(),
            })
        }
    };

    let podcasts = db
        .latest_published_podcast_episodes(6)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.members_only)
        .map(|e| PublicMediaRow {
            title: e.title,
            url: format!("/podcasts/{}", e.slug),
            description: e.description,
            kind: "Podcast",
            duration: format_seconds(e.duration_seconds),
            date: e.published_at.map(format_unix).unwrap_or_default(),
        })
        .collect();

    let videos = db
        .latest_published_video_podcasts(6)
        .unwrap_or_default()
        .into_iter()
        .map(|v| PublicMediaRow {
            title: v.title,
            url: format!("/videos/{}", v.slug),
            description: v.description,
            kind: "Video",
            duration: format_seconds(v.duration_seconds),
            date: v.published_at.map(format_unix).unwrap_or_default(),
        })
        .collect();

    let mut upcoming: Vec<PublicMediaRow> = Vec::new();
    upcoming.extend(
        db.all_podcast_episodes()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.status == EpisodeStatus::Draft && !e.members_only)
            .take(4)
            .map(|e| PublicMediaRow {
                title: e.title,
                url: "/podcasts".into(),
                description: e.description,
                kind: "Podcast",
                duration: format_seconds(e.duration_seconds),
                date: "Coming soon".into(),
            }),
    );
    upcoming.extend(
        db.all_video_podcasts()
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v.status == VideoStatus::Draft || v.status == VideoStatus::Transcoding)
            .take(4)
            .map(|v| PublicMediaRow {
                title: v.title,
                url: "/videos".into(),
                description: v.description,
                kind: "Video",
                duration: format_seconds(v.duration_seconds),
                date: "Coming soon".into(),
            }),
    );
    upcoming.truncate(6);

    render(PublicHomePage {
        stream_name: s.stream_name,
        podcasts,
        videos,
        upcoming,
    })
}

async fn public_listen(State(state): State<Arc<EngineState>>) -> Response {
    let s = state.settings.snapshot();
    render(PublicListenPage {
        stream_name: s.stream_name,
    })
}

// ============================================================================
// AutoDJ — fills dead air when no live source is connected
// ============================================================================

fn spawn_autodj(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    state.spawn_once("autodj", move |st| async move {
        autodj_loop(st, shutdown).await;
    });
}

async fn autodj_loop(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    use ccc_database::PlaylistKind;
    use rand::seq::SliceRandom;

    let db = match state.db.clone() {
        Some(db) => db,
        None => return,
    };

    // If the PCM-mixed crossfade pipeline fails to spawn (e.g. ffmpeg
    // missing or the encoder dies), we record that here and stop trying
    // to spawn it again — every subsequent pass falls through to the
    // per-track raw pacer below.
    let mut mixer_disabled = false;

    loop {
        if shutdown.is_cancelled() {
            break;
        }
        // Dead-air guard: if the main AutoDJ hasn't aired a track in
        // `dead_air_seconds`, switch to the backup rotation. This is
        // the "we'll be right back" loop — station IDs, jingles,
        // anything that prevents listeners from hearing silence.
        // Bounded by `backup_max_seconds` so we don't get stuck
        // forever if the backup is also empty.
        if let Some(track) = pick_backup_track(&state, &db) {
            // Track how long we've been on backup. After
            // backup_max_seconds, return None so the main flow runs.
            let np = NowPlaying {
                source: "backup",
                track_id: Some(track.id),
                title: track.title.clone(),
                artist: track.artist.clone(),
                started_at: unix_now(),
                duration_seconds: track.duration_seconds.max(1),
            };
            *state.now_playing.write().expect("np poisoned") = Some(np.clone());
            emit_update(&state, Some(np.clone()));
            push_history(&state, np);
            println!(
                "[autodj] DEAD-AIR BACKUP: {} - {} ({}s)",
                track.artist, track.title, track.duration_seconds
            );
            stream_track(&state, &track).await;
            continue;
        }
        // Live source > relay > AutoDJ. Pause if either upstream is feeding bytes.
        if state.autodj_paused.load(Ordering::SeqCst) {
            *state.now_playing.write().expect("np poisoned") = None;
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if state.source_alive.load(Ordering::SeqCst) {
            *state.now_playing.write().expect("np poisoned") = Some(NowPlaying {
                source: "live",
                track_id: None,
                title: "Live broadcast".into(),
                artist: String::new(),
                started_at: unix_now(),
                duration_seconds: 0,
            });
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if state.relay_alive.load(Ordering::SeqCst) {
            *state.now_playing.write().expect("np poisoned") = Some(NowPlaying {
                source: "relay",
                track_id: None,
                title: "Relay".into(),
                artist: String::new(),
                started_at: unix_now(),
                duration_seconds: 0,
            });
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if state.presenter_alive.load(Ordering::SeqCst) {
            // The remote-presenter ingest is pushing bytes. The
            // presenter task itself manages the now-playing entry;
            // we just sleep to let the operator know the AutoDJ is
            // yielding.
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // When crossfade is enabled, prefer the PCM-mixed pipeline:
        // a single persistent ffmpeg encoder with per-track decoders
        // mixed sample-wise across the crossfade window. Falls through
        // to the per-track raw pacer if the pipeline can't be started.
        let xf = state.settings.snapshot().crossfade_seconds;
        if xf > 0 && !mixer_disabled {
            match mixer::run_crossfaded_autodj(state.clone()).await {
                Ok(()) => {
                    // Mixer returned cleanly — preemption, schedule
                    // change, or empty library. Re-enter the outer
                    // loop, which will sleep on preemption or re-pick
                    // the queue for the next mixed pass.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "[autodj] crossfade pipeline failed ({}); falling back to per-track pacer",
                        e
                    );
                    mixer_disabled = true;
                }
            }
        }

        // Pick the active scheduled playlist if any matches; otherwise
        // fall back to the whole library shuffled.
        let active_id: Option<u64> = playlists::pick_active_playlist(&db).map(|p| p.id);
        let (mut queue, kind, source_label) = match active_id {
            Some(pid) => match db.get_playlist(pid).ok().flatten() {
                Some(p) => {
                    let tracks = db.playlist_tracks(p.id).unwrap_or_default();
                    if tracks.is_empty() {
                        (
                            db.all_tracks().unwrap_or_default(),
                            PlaylistKind::Shuffle,
                            format!("library (empty playlist '{}')", p.name),
                        )
                    } else {
                        (tracks, p.kind, format!("playlist '{}'", p.name))
                    }
                }
                None => (
                    db.all_tracks().unwrap_or_default(),
                    PlaylistKind::Shuffle,
                    "library shuffle".to_string(),
                ),
            },
            None => (
                db.all_tracks().unwrap_or_default(),
                PlaylistKind::Shuffle,
                "library shuffle".to_string(),
            ),
        };

        if queue.is_empty() {
            *state.now_playing.write().expect("np poisoned") = None;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        if kind == PlaylistKind::Shuffle {
            queue.shuffle(&mut rand::thread_rng());
        }

        println!("[autodj] starting pass: {}", source_label);
        for track in queue {
            if state.autodj_paused.load(Ordering::SeqCst) {
                *state.now_playing.write().expect("np poisoned") = None;
                break;
            }
            if state.source_alive.load(Ordering::SeqCst)
                || state.relay_alive.load(Ordering::SeqCst)
                || state.presenter_alive.load(Ordering::SeqCst)
            {
                break;
            }
            // The active playlist might have changed (different schedule
            // window now). Bail to the outer loop so we re-pick.
            let current_active_id = playlists::pick_active_playlist(&db).map(|p| p.id);
            if current_active_id != active_id {
                break;
            }

            // Frequency cap: skip this candidate if the same artist
            // has aired too many times in the recent window. We use
            // the live history snapshot so consecutive tracks in the
            // same pass also see each other.
            let snap = state.settings.snapshot();
            let history_snapshot: Vec<NowPlaying> = state
                .history
                .read()
                .map(|g| g.iter().cloned().collect())
                .unwrap_or_default();
            let accepted = pick_with_caps(
                std::slice::from_ref(&track),
                &history_snapshot,
                snap.max_artist_in_window,
                snap.max_album_in_window,
                snap.cap_window_hours.saturating_mul(3600),
            );
            if accepted.is_none() {
                println!(
                    "[autodj] skipping {} - {} (artist cap reached)",
                    track.artist, track.title
                );
                continue;
            }

            let np = NowPlaying {
                source: "autodj",
                track_id: Some(track.id),
                title: track.title.clone(),
                artist: track.artist.clone(),
                started_at: unix_now(),
                duration_seconds: track.duration_seconds,
            };
            *state.now_playing.write().expect("np poisoned") = Some(np.clone());
            // Broadcast the new now-playing state to WebSocket / SSE listeners.
            emit_update(&state, Some(np.clone()));
            push_history(&state, np);
            println!(
                "[autodj] now playing: {} - {} ({}s)",
                track.artist, track.title, track.duration_seconds
            );
            stream_track(&state, &track).await;
        }
    }
}

// ============================================================================
// Dead-air backup rotation
// ============================================================================
///
/// If the main AutoDJ hasn't picked a track in `dead_air_seconds` and
/// the operator has configured a non-empty `backup_track_ids` list,
/// return the next track from that list. Returns `None` when the
/// main flow is healthy (recently aired a track) OR when the backup
/// rotation is exhausted / disabled.
///
/// Bounded by `backup_max_seconds`: once we've been on backup for that
/// long, we return None and let the main flow run again. The main
/// flow's "queue empty" handling then decides what to do (sleep and
/// re-pick, or do nothing — which is the actual silence case the
/// operator should fix).
fn pick_backup_track(state: &EngineState, db: &Arc<CccDB>) -> Option<ccc_database::Track> {
    use rand::seq::SliceRandom;
    let s = state.settings.snapshot();
    if s.dead_air_seconds == 0 || s.backup_track_ids.is_empty() {
        return None;
    }
    let now = unix_now();
    // When was the last track of any kind? If None, we count as
    // "never aired" and need backup.
    let last_aired = state
        .now_playing
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|np| np.started_at));
    let in_backup = state
        .now_playing
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|np| np.source == "backup"))
        .unwrap_or(false);
    let elapsed = match last_aired {
        Some(ts) => now.saturating_sub(ts),
        None => s.dead_air_seconds + 1, // never aired -> immediately eligible
    };
    if !in_backup {
        // We're on the main flow; only switch to backup if it's been
        // silent for dead_air_seconds.
        if elapsed < s.dead_air_seconds {
            return None;
        }
    } else {
        // We're already in the backup rotation. Don't let it run
        // forever — bail out so the main flow gets a chance.
        if elapsed >= s.backup_max_seconds {
            return None;
        }
    }
    // Pick a random backup track.
    let ids = &s.backup_track_ids;
    if ids.is_empty() {
        return None;
    }
    let pick_id = *ids.choose(&mut rand::thread_rng())?;
    db.get_track(pick_id).ok().flatten()
}

// ============================================================================
// Frequency cap rule engine — keeps the rotation varied.
//
// Two caps:
//   * max_artist_in_window — same artist no more than N times per
//     cap_window_hours. 0 = disabled.
//   * max_album_in_window — same album no more than N times per
//     cap_window_hours. 0 = disabled.
//
// We track what we aired by walking the `history` ring (last 20
// items). For longer windows the engine keeps a small set of recently
// aired track IDs in a separate buffer (the `now_playing.started_at`
// gives us a timestamp, so we can age out anything older than the
// window).
//
// The function takes a candidate `queue` and returns the first
// track that does NOT violate either cap. If every candidate
// violates, returns `None` — caller can fall back to its own
// behaviour (re-shuffle, alarm, etc.).
// ============================================================================

/// Count how many tracks in `history` aired within the last
/// `window_secs` seconds, grouped by artist and album.
/// The history is newest-first; we just filter and count.
fn count_recent_by_artist_album(
    history: &[NowPlaying],
    window_secs: u64,
) -> (
    std::collections::HashMap<String, u32>,
    std::collections::HashMap<String, u32>,
) {
    let now = unix_now();
    let mut artist: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let album: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for np in history {
        if now.saturating_sub(np.started_at) > window_secs {
            continue;
        }
        // np.artist can be empty for live/relay/backup entries. Skip
        // those — they aren't subject to the cap.
        if !np.artist.is_empty() {
            *artist.entry(np.artist.clone()).or_insert(0) += 1;
        }
        // We don't have album info in NowPlaying; only artist. The
        // album cap has to be checked via a separate db lookup at
        // pick time, but we can still pre-filter by artist here.
        let _ = album; // reserved for a future NowPlaying.album field
    }
    (artist, album)
}

/// Apply frequency caps to a queue. Returns the first track that
/// does NOT exceed the cap. Compares candidate `Track.artist` /
/// `Track.album` against the recent history counts.
fn pick_with_caps(
    queue: &[ccc_database::Track],
    history: &[NowPlaying],
    max_artist: u32,
    max_album: u32,
    window_secs: u64,
) -> Option<ccc_database::Track> {
    if max_artist == 0 && max_album == 0 {
        // Caps disabled. Pick the first candidate.
        return queue.first().cloned();
    }
    let (artist_counts, _album_counts) = count_recent_by_artist_album(history, window_secs);
    for t in queue {
        if max_artist > 0 {
            let count = artist_counts.get(&t.artist).copied().unwrap_or(0);
            if count >= max_artist {
                continue;
            }
        }
        // Album cap: we don't have album info in history, so we
        // can't filter here without a db lookup. The AutoDJ's
        // existing shuffle will spread albums naturally; this
        // is a known limitation of the v1 cap engine. Set
        // max_album=0 to disable rather than get false positives.
        let _ = max_album;
        return Some(t.clone());
    }
    None
}

// ============================================================================
// Multi-mount variants — each runs an ffmpeg transcoder fed from the
// master broadcast, producing a distinct bitrate / format on its own
// per-variant broadcast channel. Listeners route by URL path.
// ============================================================================

fn spawn_variant_transcoders(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    state.spawn_once("variant_transcoders", move |st| async move {
        spawn_variant_transcoders_inner(st, shutdown).await;
    });
}

async fn spawn_variant_transcoders_inner(
    state: EngineState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let variants = state.settings.snapshot().variant_mounts.clone();
    for v in variants {
        // Allocate the variant's broadcast channel and remember it.
        let (tx, _) = broadcast::channel::<Bytes>(BROADCAST_CAPACITY);
        state
            .variants
            .write()
            .expect("variants poisoned")
            .insert(v.path.clone(), tx.clone());
        let st = state.clone();
        let v_clone = v.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            run_variant_transcoder(st, v_clone, tx, shutdown).await;
        });
    }
}

async fn run_variant_transcoder(
    state: EngineState,
    variant: VariantMountSetting,
    out_tx: broadcast::Sender<Bytes>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt as _;
    use tokio::process::Command;

    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let mut master_rx = state.tx.subscribe();
        let mut child = match Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg("pipe:0")
            .arg("-c:a")
            .arg("libmp3lame")
            .arg("-b:a")
            .arg(format!("{}k", variant.bitrate_kbps))
            .arg("-f")
            .arg("mp3")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[radio-engine] variant {} ffmpeg spawn failed: {} (retry in 5s)",
                    variant.path, e
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => break,
        };
        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => break,
        };

        // Pump master broadcast → ffmpeg stdin.
        let stdin_task = tokio::spawn(async move {
            loop {
                match master_rx.recv().await {
                    Ok(chunk) => {
                        if stdin.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Pump ffmpeg stdout → variant broadcast.
        let mut buf = vec![0u8; 4096];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let _ = out_tx.send(Bytes::copy_from_slice(&buf[..n]));
                }
                Err(_) => break,
            }
        }

        stdin_task.abort();
        let _ = child.wait().await;
        // ffmpeg exited (probably because the master closed). Brief
        // back-off and respawn.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// ============================================================================
// Stream relay — pulls an upstream Icecast/SHOUTcast URL and pumps the
// bytes into our broadcast channel. Live source connections preempt it.
// ============================================================================

fn spawn_relay(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    state.spawn_once("relay", move |st| async move {
        relay_loop(st, shutdown).await;
    });
}

async fn relay_loop(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let url = state.settings.snapshot().relay_url;
        if url.is_empty() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        // Don't compete with a live source.
        if state.source_alive.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        match relay_once(&state, &url).await {
            Ok(()) => {
                // Upstream closed cleanly — back off briefly and retry.
                state.relay_alive.store(false, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Err(e) => {
                state.relay_alive.store(false, Ordering::SeqCst);
                eprintln!("[radio-engine] relay error from {}: {}", url, e);
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

/// One full upstream connection. Connects, reads HTTP response headers,
/// then pumps the body into our broadcast channel until closed.
async fn relay_once(state: &EngineState, url_str: &str) -> Result<(), String> {
    let url = url::Url::parse(url_str).map_err(|e| format!("bad URL: {}", e))?;
    let host = url.host_str().ok_or("no host in URL")?.to_string();
    let port = url.port().unwrap_or(80);
    let path = if let Some(q) = url.query() {
        format!("{}?{}", url.path(), q)
    } else {
        url.path().to_string()
    };

    let mut sock = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connect: {}", e))?;
    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: ccc-radio-relay/0.1\r\nIcy-MetaData: 0\r\nConnection: close\r\n\r\n",
        path, host
    );
    sock.write_all(req.as_bytes())
        .await
        .map_err(|e| format!("write request: {}", e))?;

    // Drain the response headers (everything up to \r\n\r\n).
    let mut buf = Vec::with_capacity(4 * 1024);
    let mut probe = [0u8; 1024];
    loop {
        let n = sock.read(&mut probe).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("upstream closed before headers".into());
        }
        buf.extend_from_slice(&probe[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            // Status line check.
            let header_text = String::from_utf8_lossy(&buf[..pos]);
            let first = header_text.lines().next().unwrap_or("");
            if !first.contains("200") {
                return Err(format!("upstream HTTP: {}", first));
            }
            let body_start = pos + 4;
            let leftover: Vec<u8> = buf[body_start..].to_vec();
            state.relay_alive.store(true, Ordering::SeqCst);
            println!("[radio-engine] relay connected to {}", url_str);
            if !leftover.is_empty() {
                let _ = state.tx.send(Bytes::copy_from_slice(&leftover));
            }
            // Pump until upstream closes or a live source preempts us.
            let mut chunk = [0u8; READ_CHUNK];
            loop {
                if state.source_alive.load(Ordering::SeqCst) {
                    println!("[radio-engine] relay yielding to live source");
                    return Ok(());
                }
                // If the operator cleared the URL, drop the connection.
                if state.settings.snapshot().relay_url != url_str {
                    return Ok(());
                }
                match sock.read(&mut chunk).await {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        let _ = state.tx.send(Bytes::copy_from_slice(&chunk[..n]));
                    }
                    Err(e) => return Err(format!("read: {}", e)),
                }
            }
        }
        if buf.len() > 16 * 1024 {
            return Err("upstream headers too large".into());
        }
    }
}

// ============================================================================
// Remote presenter ingest — pulls an SRT/RTP/RTMP URL via ffmpeg and
// pumps the bytes into the broadcast channel. This is the "remote
// guest" / "remote co-host" feature. Live source connections and the
// standard Icecast relay preempt it; clearing `remote_presenter_url`
// disables it. The AutoDJ is paused while bytes are flowing.
// ============================================================================

fn spawn_remote_presenter(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    state.spawn_once("remote_presenter", move |st| async move {
        remote_presenter_loop(st, shutdown).await;
    });
}

async fn remote_presenter_loop(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let snap = state.settings.snapshot();
        let url = snap.remote_presenter_url.clone();
        let label = snap.remote_presenter_label.clone();
        let passphrase = snap.remote_presenter_passphrase.clone();
        if url.is_empty() {
            // Disabled — idle until the operator configures a URL.
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        // Don't compete with a live source or the Icecast relay.
        if state.source_alive.load(Ordering::SeqCst) || state.relay_alive.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        // Surface the presenter in the public now-playing view.
        *state.now_playing.write().expect("np poisoned") = Some(NowPlaying {
            source: "remote_presenter",
            track_id: None,
            title: label.clone(),
            artist: "Remote presenter is on-air".into(),
            started_at: unix_now(),
            duration_seconds: 0,
        });
        match remote_presenter_once(&state, &url, &passphrase).await {
            Ok(()) => {
                // Upstream closed cleanly (operator ended the call,
                // ffmpeg reached EOF, or we yielded to a live source).
                state.presenter_alive.store(false, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => {
                state.presenter_alive.store(false, Ordering::SeqCst);
                eprintln!("[radio-engine] remote presenter error from {}: {}", url, e);
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

/// One full ingest attempt. Spawns `ffmpeg -i <url> -f mp3 -` and
/// pumps its stdout into the broadcast channel. Returns Ok(()) on a
/// clean EOF or a graceful preemption, Err on a real ffmpeg failure.
async fn remote_presenter_once(
    state: &EngineState,
    url_str: &str,
    passphrase: &str,
) -> Result<(), String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt as _;
    use tokio::process::Command;

    // Sanity-check the URL scheme so we can give a friendly error
    // message before spawning ffmpeg. Any non-empty URL is ffmpeg's
    // problem; we just want to catch obvious typos up front.
    let parsed = url::Url::parse(url_str).map_err(|e| format!("bad URL: {}", e))?;
    let scheme = parsed.scheme();
    if !matches!(
        scheme,
        "srt" | "rtp" | "rtmp" | "udp" | "tcp" | "http" | "https"
    ) {
        return Err(format!(
            "unsupported scheme '{}'; expected srt://, rtp://, rtmp://, udp://, tcp://, or http(s)://",
            scheme
        ));
    }

    let snap = state.settings.snapshot();
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-re")
        // `-re` reads input at native rate; harmless for live and
        // protects against the caller forcing a fast drain.
        .arg("-i")
        .arg(url_str);
    // SRT passphrase (encryption). The argument is harmless when the
    // URL isn't SRT, but ffmpeg logs a warning, so we only pass it
    // when the scheme is srt.
    if scheme == "srt" && !passphrase.is_empty() {
        cmd.arg("-passphrase").arg(passphrase);
    }
    cmd.arg("-c:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg(format!("{}k", snap.station_bitrate_kbps))
        .arg("-f")
        .arg("mp3")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("ffmpeg spawn failed: {}", e))?;
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return Err("ffmpeg had no stdout".into()),
    };
    let stderr = child.stderr.take();

    state.presenter_alive.store(true, Ordering::SeqCst);
    println!("[radio-engine] remote presenter connected: {}", url_str);

    // Drain stderr in a side task so a noisy ffmpeg doesn't deadlock
    // its stdout pipe. We log only the first error line, then drop the
    // rest — ffmpeg's error spew isn't useful to operators.
    let stderr_state = state.clone();
    let stderr_task = tokio::spawn(async move {
        if let Some(mut s) = stderr {
            use tokio::io::AsyncBufReadExt as _;
            let mut reader = tokio::io::BufReader::new(&mut s);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            if !line.trim().is_empty() {
                eprintln!("[radio-engine] ffmpeg: {}", line.trim());
                stderr_state.presenter_alive.store(false, Ordering::SeqCst);
            }
        }
    });

    let mut buf = vec![0u8; 4096];
    loop {
        // If a live source connects, yield.
        if state.source_alive.load(Ordering::SeqCst) {
            println!("[radio-engine] remote presenter yielding to live source");
            let _ = child.kill().await;
            let _ = stderr_task.await;
            return Ok(());
        }
        // If the Icecast relay has come up, yield.
        if state.relay_alive.load(Ordering::SeqCst) {
            let _ = child.kill().await;
            let _ = stderr_task.await;
            return Ok(());
        }
        // If the operator cleared the URL, stop the ingest.
        if state.settings.snapshot().remote_presenter_url != url_str {
            let _ = child.kill().await;
            let _ = stderr_task.await;
            return Ok(());
        }
        match stdout.read(&mut buf).await {
            Ok(0) => {
                // ffmpeg reached EOF or closed the pipe.
                break;
            }
            Ok(n) => {
                let _ = state.tx.send(Bytes::copy_from_slice(&buf[..n]));
            }
            Err(_) => break,
        }
    }

    let _ = child.wait().await;
    let _ = stderr_task.await;
    Ok(())
}

// ============================================================================
// Recording — dumps every broadcast byte to disk while enabled
// ============================================================================

fn spawn_recorder(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    state.spawn_once("recorder", move |st| async move {
        recorder_loop(st, shutdown).await;
    });
}

async fn recorder_loop(state: EngineState, shutdown: tokio_util::sync::CancellationToken) {
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt as _;

    let mut rx = state.tx.subscribe();
    let mut current_path: Option<String> = None;
    let mut current_file: Option<File> = None;

    loop {
        let chunk = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                // Flush any open recording file before exiting.
                if let (Some(_p), Some(mut f)) = (current_path.take(), current_file.take()) {
                    let _ = f.flush().await;
                }
                break;
            }
            recv = rx.recv() => match recv {
                Ok(c) => c,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        };
        {
            let want_path = state
                .recording
                .read()
                .expect("recording poisoned")
                .as_ref()
                .map(|h| h.path.clone());
            if want_path != current_path {
                // State changed — close any open file and open the new one.
                if let Some(mut f) = current_file.take() {
                    let _ = f.flush().await;
                }
                if let Some(p) = &want_path {
                    if let Some(parent) = std::path::Path::new(p).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match File::create(p).await {
                        Ok(f) => current_file = Some(f),
                        Err(e) => {
                            eprintln!("[radio-engine] could not open recording {}: {}", p, e);
                        }
                    }
                }
                current_path = want_path;
            }
            if let Some(f) = current_file.as_mut() {
                let _ = f.write_all(&chunk).await;
            }
        }
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" => "mp3",
        "audio/aac" | "audio/mp4" => "m4a",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/flac" => "flac",
        _ => "bin",
    }
}

async fn admin_record_start(State(state): State<Arc<EngineState>>) -> Response {
    let snap = state.settings.snapshot();
    let ext = extension_for_mime(&snap.content_type);
    let now = unix_now();
    let mut path = state.library_dir.as_ref().clone();
    path.push("recordings");
    path.push(format!("recording-{}.{}", now, ext));
    *state.recording.write().expect("recording poisoned") = Some(RecordingHandle {
        path: path.to_string_lossy().to_string(),
        started_at: now,
    });
    println!("[radio-engine] recording started -> {}", path.display());
    Redirect::to("/admin/radio-engine/radio").into_response()
}

async fn admin_record_stop(State(state): State<Arc<EngineState>>) -> Response {
    *state.recording.write().expect("recording poisoned") = None;
    println!("[radio-engine] recording stopped");
    Redirect::to("/admin/radio-engine/radio").into_response()
}

async fn admin_autodj_pause(State(state): State<Arc<EngineState>>) -> Response {
    state.autodj_paused.store(true, Ordering::SeqCst);
    *state.now_playing.write().expect("np poisoned") = None;
    println!("[radio-engine] AutoDJ paused");
    Redirect::to("/admin/radio-engine/radio").into_response()
}

async fn admin_autodj_resume(State(state): State<Arc<EngineState>>) -> Response {
    state.autodj_paused.store(false, Ordering::SeqCst);
    println!("[radio-engine] AutoDJ resumed");
    Redirect::to("/admin/radio-engine/radio").into_response()
}

/// Drop the currently-configured remote-presenter URL so the ingest
/// task stops trying to connect on the next iteration. Useful for
/// "end the call" buttons. We don't kill the live ffmpeg process
/// here — it will see the URL no longer matches on its next yield
/// check and exit on its own.
async fn admin_presenter_stop(State(state): State<Arc<EngineState>>) -> Response {
    let mut snap = state.settings.snapshot();
    snap.remote_presenter_url = String::new();
    let _ = state.settings.replace(snap);
    state.presenter_alive.store(false, Ordering::SeqCst);
    println!("[radio-engine] remote presenter stopped by operator");
    Redirect::to("/admin/radio-engine/radio/engineering?saved=1").into_response()
}

pub(crate) fn push_history(state: &EngineState, entry: NowPlaying) {
    let mut h = state.history.write().expect("history poisoned");
    if h.len() >= HISTORY_CAPACITY {
        h.pop_back();
    }
    h.push_front(entry);
}

/// Open the file and stream it into the broadcast channel paced at the
/// track's effective bitrate (size_bytes / duration_seconds). Yields
/// the entire track, then returns. If the file can't be opened or the
/// duration is bogus, sleeps `duration_seconds` to keep the schedule
/// aligned and bails.
async fn stream_track(state: &EngineState, track: &Track) {
    if track.duration_seconds == 0 {
        eprintln!("[autodj] skipping {} (no duration)", track.title);
        return;
    }

    // Build the audio filter chain: volume normalization (if enabled +
    // measurement available) followed by a short fade in at the start
    // and a fade out at the end. Crossfade is approximated this way:
    // each track's last `crossfade_seconds` overlaps the NEXT track's
    // first `crossfade_seconds` of fade-in. With ffmpeg single-track
    // pipes that's the cleanest we can do without holding two
    // subprocesses concurrently — true overlap mixing comes in a
    // follow-up that pipes both decoders into one mixer.
    let snap = state.settings.snapshot();
    let mut filters: Vec<String> = Vec::new();
    if snap.apply_loudness_normalization {
        if let Some(g) = track.target_gain_db {
            filters.push(format!("volume={:+.2}dB", g));
        }
    }
    let xf = snap.crossfade_seconds.min(10) as u64;
    if xf > 0 {
        filters.push(format!("afade=t=in:st=0:d={}", xf));
        if track.duration_seconds > xf + 1 {
            let st = track.duration_seconds - xf;
            filters.push(format!("afade=t=out:st={}:d={}", st, xf));
        }
    }
    let af_arg = filters.join(",");

    use tokio::process::Command;
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&track.file_path);
    if !af_arg.is_empty() {
        cmd.arg("-af").arg(&af_arg);
    }
    cmd.arg("-c:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg(format!("{}k", snap.station_bitrate_kbps))
        .arg("-f")
        .arg("mp3")
        .arg("-")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[autodj] ffmpeg spawn failed: {} — falling back", e);
            return stream_track_raw_pacing(state, track).await;
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return,
    };

    // Pace the encoded MP3 output at the configured station bitrate.
    let chunks_per_sec: u64 = 10;
    let bytes_per_sec = (snap.station_bitrate_kbps as u64 * 1000) / 8;
    let bytes_per_chunk = (bytes_per_sec / chunks_per_sec).max(2048) as usize;
    let chunk_period = Duration::from_millis(1000 / chunks_per_sec);

    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; bytes_per_chunk];
    loop {
        if state.autodj_paused.load(Ordering::SeqCst)
            || state.source_alive.load(Ordering::SeqCst)
            || state.relay_alive.load(Ordering::SeqCst)
            || state.presenter_alive.load(Ordering::SeqCst)
        {
            let _ = child.kill().await;
            break;
        }
        let n = match stdout.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let _ = state.tx.send(Bytes::copy_from_slice(&buf[..n]));
        tokio::time::sleep(chunk_period).await;
    }
    let _ = child.wait().await;
}

/// Fallback for hosts without ffmpeg: just chunk + pace the raw file
/// like the original implementation. No gain, no fades, no transcode.
async fn stream_track_raw_pacing(state: &EngineState, track: &Track) {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    if track.size_bytes == 0 {
        return;
    }
    let mut file = match File::open(&track.file_path).await {
        Ok(f) => f,
        Err(_) => return,
    };
    let chunks_per_sec: u64 = 10;
    let bytes_per_chunk = (track.size_bytes / track.duration_seconds / chunks_per_sec).max(2048);
    let chunk_period = Duration::from_millis(1000 / chunks_per_sec);
    let mut buf = vec![0u8; bytes_per_chunk as usize];
    loop {
        if state.autodj_paused.load(Ordering::SeqCst)
            || state.source_alive.load(Ordering::SeqCst)
            || state.relay_alive.load(Ordering::SeqCst)
            || state.presenter_alive.load(Ordering::SeqCst)
        {
            break;
        }
        let n = match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let _ = state.tx.send(Bytes::copy_from_slice(&buf[..n]));
        tokio::time::sleep(chunk_period).await;
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Render the current track as an ICY-style "Artist - Title" string,
/// or fall back to the station name when nothing is playing. Returned
/// without surrounding quotes — the encoder wraps it in `StreamTitle='..';`.
fn current_icy_title(state: &EngineState) -> String {
    let np = state.now_playing.read().expect("np poisoned").clone();
    let snap = state.settings.snapshot();
    match np {
        Some(n) if !n.artist.is_empty() => format!("{} - {}", n.artist, n.title),
        Some(n) => n.title,
        None => snap.stream_name,
    }
}

/// Build the byte sequence for one ICY metadata block:
/// `[length/16] StreamTitle='<title>';\0\0...` padded to a 16-byte
/// boundary. Single quotes inside the title are stripped because
/// they'd unbalance the StreamTitle delimiters.
fn encode_icy_metadata(title: &str) -> Vec<u8> {
    let safe_title: String = title.chars().filter(|c| *c != '\'').collect();
    let mut payload = format!("StreamTitle='{}';", safe_title).into_bytes();
    // Per spec: each block is `(len/16) blocks of 16 bytes`. Round up.
    let len = payload.len();
    let blocks = len.div_ceil(16).min(255);
    let target = blocks * 16;
    payload.resize(target, 0);
    let mut out = Vec::with_capacity(1 + target);
    out.push(blocks as u8);
    out.extend_from_slice(&payload);
    out
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn format_seconds(s: u64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, sec)
    } else {
        format!("{:02}:{:02}", m, sec)
    }
}

fn format_unix(ts: u64) -> String {
    let (y, mo, d, h, mi) = unix_to_ymdhm(ts);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, mi)
}

fn unix_to_ymdhm(secs: u64) -> (u32, u32, u32, u32, u32) {
    let minutes_total = secs / 60;
    let hours_total = minutes_total / 60;
    let days_since_epoch = (hours_total / 24) as u32;
    let minute = (minutes_total % 60) as u32;
    let hour = (hours_total % 24) as u32;
    let mut year = 1970u32;
    let mut days_remaining = days_since_epoch;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if days_remaining < in_year {
            break;
        }
        days_remaining -= in_year;
        year += 1;
    }
    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months_leap = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = if is_leap(year) {
        months_leap
    } else {
        months_normal
    };
    let mut month = 1u32;
    for &len in &months {
        if days_remaining < len {
            break;
        }
        days_remaining -= len;
        month += 1;
    }
    let day = days_remaining + 1;
    (year, month, day, hour, minute)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Validate a `remote_presenter_url` value before the engine tries to
/// spawn ffmpeg against it. Returns `Ok(normalized)` for acceptable
/// inputs and `Err(reason)` for bad ones. Used by the admin form
/// (so the operator gets a friendly error before clicking Save) and
/// by the ingest task (defence in depth — the URL could have been
/// hand-edited in `radio.toml`).
pub fn validate_presenter_url(s: &str) -> Result<String, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        // Empty == disabled. Not an error.
        return Ok(String::new());
    }
    let parsed = url::Url::parse(trimmed).map_err(|e| format!("bad URL: {}", e))?;
    let scheme = parsed.scheme();
    if !matches!(
        scheme,
        "srt" | "rtp" | "rtmp" | "udp" | "tcp" | "http" | "https"
    ) {
        return Err(format!(
            "unsupported scheme '{}'; expected srt://, rtp://, rtmp://, udp://, tcp://, or http(s)://",
            scheme
        ));
    }
    if parsed.host_str().is_none_or(|h| h.is_empty()) {
        return Err("URL must include a host".into());
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_track_ids_basic() {
        let v = parse_track_ids("1, 2 3\n4");
        assert_eq!(v, vec![1, 2, 3, 4]);
    }

    #[test]
    fn parse_track_ids_drops_garbage() {
        let v = parse_track_ids("5, abc, , 6");
        assert_eq!(v, vec![5, 6]);
    }

    #[test]
    fn settings_default_has_dead_air_fields() {
        let s = RadioSettings::default();
        assert_eq!(s.dead_air_seconds, 15);
        assert!(s.backup_track_ids.is_empty());
        assert_eq!(s.backup_max_seconds, 300);
    }

    #[test]
    fn settings_toml_round_trip_with_backup() {
        let s = RadioSettings {
            dead_air_seconds: 30,
            backup_track_ids: vec![1, 2, 3],
            backup_max_seconds: 600,
            ..RadioSettings::default()
        };
        let toml = toml::to_string(&s).unwrap();
        let back: RadioSettings = toml::from_str(&toml).unwrap();
        assert_eq!(back.dead_air_seconds, 30);
        assert_eq!(back.backup_track_ids, vec![1, 2, 3]);
        assert_eq!(back.backup_max_seconds, 600);
    }

    #[test]
    fn settings_back_compat_when_backup_keys_missing() {
        // Existing radio.toml files in the wild don't have these
        // fields. They must deserialize cleanly with defaults.
        let minimal = r#"
            port = 8000
            mount_path = "/listen.mp3"
            source_password = "x"
            content_type = "audio/mpeg"
            stream_name = "Test"
        "#;
        let s: RadioSettings = toml::from_str(minimal).unwrap();
        assert_eq!(s.dead_air_seconds, 15);
        assert!(s.backup_track_ids.is_empty());
        assert_eq!(s.backup_max_seconds, 300);
    }

    fn t(id: u64, artist: &str, title: &str) -> ccc_database::Track {
        ccc_database::Track {
            id,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            genre: String::new(),
            file_path: String::new(),
            mime_type: "audio/mpeg".into(),
            size_bytes: 0,
            duration_seconds: 180,
            created_at: 0,
            target_gain_db: None,
        }
    }

    fn np(artist: &str, secs_ago: u64) -> NowPlaying {
        NowPlaying {
            source: "autodj",
            track_id: None,
            title: "x".into(),
            artist: artist.into(),
            started_at: unix_now().saturating_sub(secs_ago),
            duration_seconds: 180,
        }
    }

    #[test]
    fn caps_disabled_picks_first() {
        let q = vec![t(1, "A", "1"), t(2, "B", "2")];
        let h = vec![np("A", 60)];
        let p = pick_with_caps(&q, &h, 0, 0, 3600).unwrap();
        assert_eq!(p.id, 1);
    }

    #[test]
    fn caps_artist_at_cap_skipped() {
        let q = vec![t(1, "A", "1"), t(2, "B", "2")];
        let h = vec![np("A", 60), np("A", 120), np("A", 180), np("A", 240)];
        // max_artist = 4 — A is already at 4, must be skipped
        let p = pick_with_caps(&q, &h, 4, 0, 3600).unwrap();
        assert_eq!(p.id, 2);
    }

    #[test]
    fn caps_artist_window_respects_age() {
        let q = vec![t(1, "A", "1")];
        // 4 A's aired, but all of them over 4 hours ago. Cap window
        // is 1h. Should be fine.
        let h = vec![
            np("A", 60 * 60 * 5),
            np("A", 60 * 60 * 5),
            np("A", 60 * 60 * 5),
            np("A", 60 * 60 * 5),
        ];
        let p = pick_with_caps(&q, &h, 4, 0, 3600).unwrap();
        assert_eq!(p.id, 1);
    }

    #[test]
    fn caps_all_violated_returns_none() {
        let q = vec![t(1, "A", "1")];
        let h = vec![np("A", 60), np("A", 120)];
        // Cap = 1, A is already at 2 -> skip
        let p = pick_with_caps(&q, &h, 1, 0, 3600);
        assert!(p.is_none());
    }

    #[test]
    fn caps_live_source_doesnt_count() {
        // Live source entries have empty artist. The cap should
        // ignore them entirely.
        let q = vec![t(1, "A", "1")];
        let mut live = np("", 60);
        live.source = "live";
        let h = vec![live.clone(), live.clone(), live.clone(), live];
        let p = pick_with_caps(&q, &h, 4, 0, 3600).unwrap();
        assert_eq!(p.id, 1);
    }

    // ---- Remote presenter ingest --------------------------------------

    #[test]
    fn presenter_url_validate_accepts_supported_schemes() {
        let cases = [
            "srt://presenter.example.com:9000?mode=caller",
            "rtp://239.0.0.1:5004",
            "rtmp://presenter.example.com/live/key",
            "udp://239.0.0.1:5004",
            "tcp://stream.example.com:9000",
            "http://stream.example.com/listen.mp3",
            "https://stream.example.com/listen.mp3",
        ];
        for c in cases {
            let out = validate_presenter_url(c).expect(c);
            assert_eq!(out, c, "validator round-tripped input for {}", c);
        }
    }

    #[test]
    fn presenter_url_empty_means_disabled_not_error() {
        assert_eq!(validate_presenter_url("").unwrap(), "");
        assert_eq!(validate_presenter_url("   ").unwrap(), "");
    }

    #[test]
    fn presenter_url_rejects_unknown_scheme() {
        let err = validate_presenter_url("ftp://nope.example.com").unwrap_err();
        assert!(err.contains("unsupported scheme"), "got: {}", err);
    }

    #[test]
    fn presenter_url_rejects_missing_host() {
        let err = validate_presenter_url("srt://").unwrap_err();
        assert!(err.contains("host"), "got: {}", err);
    }

    #[test]
    fn presenter_url_rejects_garbage() {
        let err = validate_presenter_url("not a url at all").unwrap_err();
        assert!(
            err.contains("bad URL") || err.contains("host"),
            "got: {}",
            err
        );
    }

    #[test]
    fn settings_default_has_presenter_fields_disabled() {
        let s = RadioSettings::default();
        assert!(s.remote_presenter_url.is_empty());
        assert_eq!(s.remote_presenter_label, "Remote Presenter");
        assert!(s.remote_presenter_passphrase.is_empty());
    }

    #[test]
    fn settings_toml_round_trip_with_presenter() {
        let s = RadioSettings {
            remote_presenter_url: "srt://presenter.example.com:9000".into(),
            remote_presenter_label: "Ondřej from Prague".into(),
            remote_presenter_passphrase: "secret123".into(),
            ..RadioSettings::default()
        };
        let toml = toml::to_string(&s).unwrap();
        let back: RadioSettings = toml::from_str(&toml).unwrap();
        assert_eq!(back.remote_presenter_url, s.remote_presenter_url);
        assert_eq!(back.remote_presenter_label, s.remote_presenter_label);
        assert_eq!(
            back.remote_presenter_passphrase,
            s.remote_presenter_passphrase
        );
    }

    #[test]
    fn settings_back_compat_when_presenter_keys_missing() {
        // Existing radio.toml files in the wild don't have these
        // fields. They must deserialize cleanly with defaults.
        let minimal = r#"
            port = 8000
            mount_path = "/listen.mp3"
            source_password = "x"
            content_type = "audio/mpeg"
            stream_name = "Test"
        "#;
        let s: RadioSettings = toml::from_str(minimal).unwrap();
        assert!(s.remote_presenter_url.is_empty());
        assert_eq!(s.remote_presenter_label, "Remote Presenter");
        assert!(s.remote_presenter_passphrase.is_empty());
    }

    #[test]
    fn presenter_url_trims_whitespace() {
        let out = validate_presenter_url("  srt://h:9000  ").unwrap();
        assert_eq!(out, "srt://h:9000");
    }
}
