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
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use ccc_database::{CccDB, Track};
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
            stream_name: "Creator Command Center Radio".into(),
            library_dir: default_library_dir(),
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
struct EngineState {
    settings: Arc<SettingsStore>,
    tx: broadcast::Sender<Bytes>,
    source_alive: Arc<AtomicBool>,
    listener_count: Arc<AtomicUsize>,
    /// Where uploaded music tracks live on disk.
    library_dir: Arc<PathBuf>,
    /// Live current-track snapshot for /api/radio/nowplaying + dashboard.
    now_playing: Arc<RwLock<Option<NowPlaying>>>,
    /// Live database handle (None when constructed via the legacy CLI
    /// `radio` subcommand, which has no DB).
    db: Option<Arc<CccDB>>,
}

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
            listener_count: Arc::new(AtomicUsize::new(0)),
            library_dir: Arc::new(library_dir),
            now_playing: Arc::new(RwLock::new(None)),
            db: None,
        };
        Self {
            port,
            protocol,
            state,
        }
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
            listener_count: Arc::new(AtomicUsize::new(0)),
            library_dir: Arc::new(library_dir),
            now_playing: Arc::new(RwLock::new(None)),
            db: None,
        };
        Ok(Self {
            port,
            protocol: StreamingProtocol::Icecast,
            state,
        })
    }

    /// Run the engine until something cancels (or forever). Used by the
    /// standalone `radio` CLI subcommand.
    pub async fn run(&self) -> Result<(), std::io::Error> {
        run_engine(self.state.clone()).await
    }
}

impl Plugin for RadioEnginePlugin {
    fn name(&self) -> &'static str {
        "Native Radio Engine"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let snap = self.state.settings.snapshot();
        println!(
            "Initializing Radio Engine on TCP {} (mount {})",
            snap.port, snap.mount_path
        );
        // Spawn the engine. Engine error => we log and exit the task; the
        // host stays up.
        let st = self.state.clone();
        tokio::spawn(async move {
            if let Err(e) = run_engine(st).await {
                eprintln!("[radio-engine] terminated: {}", e);
            }
        });
        Ok(())
    }

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        // Public now-playing endpoint — usable by listeners' web pages,
        // overlays, etc. JSON, not gated.
        let st = self.state.clone();
        Some(
            Router::new()
                .route("/api/radio/nowplaying", get(public_now_playing))
                .with_state(st),
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
        spawn_autodj(st.clone());
        Some(
            Router::new()
                .route("/radio", get(admin_get).post(admin_post))
                .route(
                    "/radio/tracks",
                    get(library::admin_track_list).post(library::admin_track_upload),
                )
                .route(
                    "/radio/tracks/:id/delete",
                    post(library::admin_track_delete),
                )
                .layer(axum::extract::DefaultBodyLimit::max(
                    library::MAX_TRACK_BYTES,
                ))
                .with_state(st),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Radio",
            href: "/admin/radio".into(),
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

async fn run_engine(state: EngineState) -> Result<(), std::io::Error> {
    let port = state.settings.snapshot().port;
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("[radio-engine] listening on {}", addr);

    loop {
        match listener.accept().await {
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
        println!("[radio-engine] source disconnected from {}", peer);
        Ok(())
    } else if method.eq_ignore_ascii_case("GET") && path == snap.mount_path {
        // Listener.
        let mut headers = format!(
            "HTTP/1.0 200 OK\r\n\
             Content-Type: {}\r\n\
             icy-name: {}\r\n\
             Cache-Control: no-cache\r\n\
             Connection: close\r\n",
            snap.content_type, snap.stream_name
        );
        headers.push_str("\r\n");
        if sock.write_all(headers.as_bytes()).await.is_err() {
            return Ok(());
        }

        let mut rx = state.tx.subscribe();
        state.listener_count.fetch_add(1, Ordering::SeqCst);
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    if sock.write_all(&chunk).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Listener too slow — close so they can reconnect cleanly.
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
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
    source_alive: bool,
    listeners: usize,
    port: u16,
    mount_path: String,
    source_password: String,
    content_type: String,
    stream_name: String,
    track_count: usize,
    now_playing_title: Option<String>,
    now_playing_artist: Option<String>,
    saved: Option<bool>,
}

async fn admin_get(State(state): State<EngineState>) -> Response {
    let snap = state.settings.snapshot();
    let np = state.now_playing.read().expect("np poisoned").clone();
    let track_count = state
        .db
        .as_ref()
        .and_then(|db| db.all_tracks().ok())
        .map(|v| v.len())
        .unwrap_or(0);
    render(AdminDashboardPage {
        source_alive: state.source_alive.load(Ordering::SeqCst),
        listeners: state.listener_count.load(Ordering::SeqCst),
        port: snap.port,
        mount_path: snap.mount_path,
        source_password: snap.source_password,
        content_type: snap.content_type,
        stream_name: snap.stream_name,
        track_count,
        now_playing_title: np.as_ref().map(|n| n.title.clone()),
        now_playing_artist: np.as_ref().and_then(|n| {
            if n.artist.is_empty() {
                None
            } else {
                Some(n.artist.clone())
            }
        }),
        saved: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    source_password: String,
    mount_path: String,
    content_type: String,
    stream_name: String,
}

async fn admin_post(State(state): State<EngineState>, Form(form): Form<SettingsForm>) -> Response {
    let mut snap = state.settings.snapshot();
    snap.source_password = form.source_password;
    snap.mount_path = form.mount_path.trim().to_string();
    snap.content_type = form.content_type.trim().to_string();
    snap.stream_name = form.stream_name.trim().to_string();
    let _ = state.settings.replace(snap);
    Redirect::to("/admin/radio?saved=1").into_response()
}

// ============================================================================
// Public now-playing endpoint
// ============================================================================

async fn public_now_playing(State(state): State<EngineState>) -> Response {
    let snap = state
        .now_playing
        .read()
        .expect("now_playing poisoned")
        .clone();
    Json(serde_json::json!({
        "now_playing": snap,
        "live": state.source_alive.load(Ordering::SeqCst),
        "listeners": state.listener_count.load(Ordering::SeqCst),
    }))
    .into_response()
}

// ============================================================================
// AutoDJ — fills dead air when no live source is connected
// ============================================================================

fn spawn_autodj(state: EngineState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SPAWNED: AtomicBool = AtomicBool::new(false);
    if SPAWNED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(autodj_loop(state));
}

async fn autodj_loop(state: EngineState) {
    use rand::seq::SliceRandom;

    let db = match state.db.clone() {
        Some(db) => db,
        None => return,
    };

    loop {
        // While a live source is live, the AutoDJ should not stream.
        // Sleep and re-check.
        if state.source_alive.load(Ordering::SeqCst) {
            // Mark now-playing as live (no track info — the source decides).
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

        // Build a shuffled queue of tracks. Re-shuffle each time we exhaust
        // the library so the order varies across cycles.
        let mut queue: Vec<Track> = db.all_tracks().unwrap_or_default();
        if queue.is_empty() {
            // Nothing to play — clear now-playing and back off.
            *state.now_playing.write().expect("np poisoned") = None;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        queue.shuffle(&mut rand::thread_rng());

        for track in queue {
            // Bail out of the queue on source connect — the OUTER loop
            // will pick up the live state and spin until disconnect.
            if state.source_alive.load(Ordering::SeqCst) {
                break;
            }
            *state.now_playing.write().expect("np poisoned") = Some(NowPlaying {
                source: "autodj",
                track_id: Some(track.id),
                title: track.title.clone(),
                artist: track.artist.clone(),
                started_at: unix_now(),
                duration_seconds: track.duration_seconds,
            });
            println!(
                "[autodj] now playing: {} - {} ({}s)",
                track.artist, track.title, track.duration_seconds
            );
            stream_track(&state, &track).await;
        }
    }
}

/// Open the file and stream it into the broadcast channel paced at the
/// track's effective bitrate (size_bytes / duration_seconds). Yields
/// the entire track, then returns. If the file can't be opened or the
/// duration is bogus, sleeps `duration_seconds` to keep the schedule
/// aligned and bails.
async fn stream_track(state: &EngineState, track: &Track) {
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;

    if track.duration_seconds == 0 || track.size_bytes == 0 {
        eprintln!("[autodj] skipping {} (no duration/size)", track.title);
        return;
    }
    let mut file = match File::open(&track.file_path).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[autodj] could not open {}: {}", track.file_path, e);
            tokio::time::sleep(Duration::from_secs(track.duration_seconds.min(60))).await;
            return;
        }
    };

    // Aim for ~10 chunks per second. Smaller chunks → smoother pacing,
    // smaller jitter when listeners join mid-track.
    let chunks_per_sec: u64 = 10;
    let bytes_per_chunk = (track.size_bytes / track.duration_seconds / chunks_per_sec).max(2048);
    let chunk_period = Duration::from_millis(1000 / chunks_per_sec);

    let mut buf = vec![0u8; bytes_per_chunk as usize];
    loop {
        if state.source_alive.load(Ordering::SeqCst) {
            // Live source took over mid-track — abandon.
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
