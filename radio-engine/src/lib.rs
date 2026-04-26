//! Native Rust radio streaming engine — Icecast-compatible.
//!
//! Single-mount, single-source, many-listeners. The source connects via
//! `SOURCE /mount ICE/1.0` (legacy) or `PUT /mount HTTP/1.1` (modern)
//! with HTTP Basic auth (user `source`, configured password). Audio
//! bytes from the source are fanned out via `tokio::sync::broadcast` to
//! every connected listener. Listeners that fall behind are dropped
//! (the broadcast channel skips messages instead of blocking the source).
//!
//! Co-existence with axum: the radio engine listens on its OWN TCP port
//! (typically 8000), separate from the API port. The plugin contributes
//! `admin_routes()` for status + settings on the API port.

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use base64::Engine as _;
use bytes::Bytes;
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
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
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            port: 8000,
            mount_path: "/listen.mp3".into(),
            source_password: "hackme".into(),
            content_type: "audio/mpeg".into(),
            stream_name: "Creator Command Center Radio".into(),
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
        let state = EngineState {
            settings: Arc::new(SettingsStore {
                path: PathBuf::from("radio.toml"),
                inner: RwLock::new(settings),
            }),
            tx,
            source_alive: Arc::new(AtomicBool::new(false)),
            listener_count: Arc::new(AtomicUsize::new(0)),
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
        let port = store.snapshot().port;
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = EngineState {
            settings: store,
            tx,
            source_alive: Arc::new(AtomicBool::new(false)),
            listener_count: Arc::new(AtomicUsize::new(0)),
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

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        let st = self.state.clone();
        Some(
            Router::new()
                .route("/radio", get(admin_get).post(admin_post))
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
    saved: Option<bool>,
}

async fn admin_get(State(state): State<EngineState>) -> Response {
    let snap = state.settings.snapshot();
    render(AdminDashboardPage {
        source_alive: state.source_alive.load(Ordering::SeqCst),
        listeners: state.listener_count.load(Ordering::SeqCst),
        port: snap.port,
        mount_path: snap.mount_path,
        source_password: snap.source_password,
        content_type: snap.content_type,
        stream_name: snap.stream_name,
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

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
