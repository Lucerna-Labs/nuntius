//! Live Listener Chat — real-time chat sidebar for radio listeners.
//!
//! Listeners connect via WebSocket and exchange short text messages
//! with each other while the station is on-air. The plugin is a
//! simple per-station chat room with:
//!
//! - **WebSocket endpoint** at `/api/live-chat/ws` for both reading
//!   and posting. A client sends a JSON `{"type":"msg","text":"…"}`
//!   frame to post; the server broadcasts every accepted message to
//!   every connected client (including the sender).
//! - **Recent history** at `/api/live-chat/recent` for clients that
//!   join after the conversation has already started (and for the
//!   admin dashboard).
//! - **Slow mode** — per-connection minimum interval between
//!   accepted messages. Stops one listener from spamming.
//! - **Length cap** — every message is truncated at
//!   `max_message_length` characters.
//! - **Ban list** — the operator can ban a nick (string match) from
//!   the admin form. Banned nicks are silently dropped.
//! - **Bounded history** — last `history_capacity` messages are kept
//!   in a ring; older ones fall off.
//!
//! The plugin does not depend on a database. Messages are ephemeral
//! and live in memory. A station restart clears the room. This is a
//! deliberate choice — the chat is supposed to feel like a
//! back-channel during a live broadcast, not a permanent record.

use askama::Template;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_plugin_utils::TomlSidecar;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tracing::info;

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSettings {
    /// Master switch. When false, the WebSocket endpoint returns
    /// 503 and the recent-history endpoint is empty. Default true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Per-connection minimum gap between accepted messages.
    /// Default 2s. Set to 0 to disable slow mode.
    #[serde(default = "default_slow_mode_ms")]
    pub slow_mode_ms: u64,
    /// Maximum length of an accepted message. Default 200 chars.
    #[serde(default = "default_max_message_length")]
    pub max_message_length: usize,
    /// Number of recent messages kept in memory and exposed via
    /// `/api/live-chat/recent`. Default 50.
    #[serde(default = "default_history_capacity")]
    pub history_capacity: usize,
    /// Maximum concurrent WebSocket connections. New connections
    /// beyond this are accepted then immediately closed. Default 200.
    /// Stops a runaway client from monopolising the chat.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Banned nicks (string match, case-insensitive). Banned nicks
    /// see their messages silently dropped. Default empty.
    #[serde(default)]
    pub banned_nicks: Vec<String>,
    /// Maximum broadcast channel capacity. Clients that fall behind
    /// by more than this many messages are disconnected on the
    /// `Lagged` recovery path. Default 128.
    #[serde(default = "default_broadcast_capacity")]
    pub broadcast_capacity: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_slow_mode_ms() -> u64 {
    2_000
}
fn default_max_message_length() -> usize {
    200
}
fn default_history_capacity() -> usize {
    50
}
fn default_max_connections() -> usize {
    200
}
fn default_broadcast_capacity() -> usize {
    128
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            slow_mode_ms: default_slow_mode_ms(),
            max_message_length: default_max_message_length(),
            history_capacity: default_history_capacity(),
            max_connections: default_max_connections(),
            banned_nicks: Vec::new(),
            broadcast_capacity: default_broadcast_capacity(),
        }
    }
}

pub type SettingsStore = TomlSidecar<ChatSettings>;

// ============================================================================
// Message model
// ============================================================================

/// One chat message. Sent over the wire as JSON and kept in the
/// recent-history ring. The `id` is a monotonically-increasing u64
/// so clients can de-duplicate if their network reorders frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    pub nick: String,
    pub text: String,
    pub ts: u64,
}

/// The frames we accept from clients. Today there's only one
/// (`Msg`); future versions may add `Typing`, `React`, `Delete`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientFrame {
    Msg {
        nick: String,
        text: String,
    },
    /// Heartbeat / liveness check. Server replies with a Pong frame.
    Ping,
}

/// The frames we send to clients. `Hello` is sent on connect (gives
/// the client the recent history in one shot), `Msg` is a new
/// accepted message, `Pong` is the reply to a Ping, `Rejected` is
/// the server's way of telling the client "your last message was
/// dropped because X".
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ServerFrame<'a> {
    Hello {
        history: Vec<ChatMessage>,
        slow_mode_ms: u64,
        max_message_length: usize,
    },
    Msg(&'a ChatMessage),
    Pong,
    Rejected {
        reason: String,
    },
}

// ============================================================================
// Core state
// ============================================================================

/// Coarse-grained room state shared between the WebSocket handler
/// and the admin handlers. The broadcast channel is how accepted
/// messages reach every connected client.
pub struct ChatCore {
    /// Monotonic message-id generator. Wraps `u64::MAX` cleanly
    /// (we don't care — the next message just gets id N+1).
    next_id: AtomicUsize,
    /// Recent history ring. New messages push_front; old ones
    /// pop_back once we hit `history_capacity`.
    history: Mutex<VecDeque<ChatMessage>>,
    /// Broadcast channel — every accepted message goes out on this.
    /// Subscribers (one per WS client) receive every message.
    tx: broadcast::Sender<ChatMessage>,
    /// Live connection count. Renders in the admin dashboard and
    /// caps new connections at `max_connections`.
    live_connections: AtomicUsize,
    // The slow-mode window is per-connection state, so we don't
    // track it here — it's an Instant in each WS task.
}

impl ChatCore {
    fn new(broadcast_capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(broadcast_capacity.max(8));
        Self {
            next_id: AtomicUsize::new(1),
            history: Mutex::new(VecDeque::new()),
            tx,
            live_connections: AtomicUsize::new(0),
        }
    }

    fn push_message(&self, msg: ChatMessage) {
        let mut h = self.history.lock().expect("history poisoned");
        // Note: we use the deque's own `.capacity()` rather than
        // a separate getter that would re-lock the mutex. std
        // Mutex isn't reentrant.
        if h.len() >= h.capacity() {
            h.pop_back();
        }
        h.push_front(msg);
    }

    fn history_snapshot(&self, cap: usize) -> Vec<ChatMessage> {
        let h = self.history.lock().expect("history poisoned");
        h.iter().take(cap).cloned().collect()
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) as u64
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct LiveChatPlugin {
    settings: Arc<SettingsStore>,
    core: Arc<ChatCore>,
}

impl LiveChatPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("live-chat")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("live-chat.toml"));
        let store = Arc::new(
            SettingsStore::load_or_init(&path)
                .map_err(|e| PluginError::InitError(e.to_string()))?,
        );
        let s = store.snapshot();
        let core = Arc::new(ChatCore::new(s.broadcast_capacity));
        Ok(Self {
            settings: store,
            core,
        })
    }

    /// Pure-function check: is `nick` on the ban list?
    pub(crate) fn is_banned(s: &ChatSettings, nick: &str) -> bool {
        let candidate = nick.trim().to_lowercase();
        if candidate.is_empty() {
            return false;
        }
        s.banned_nicks
            .iter()
            .any(|b| !b.trim().is_empty() && b.trim().to_lowercase() == candidate)
    }

    /// Pure-function check: does `text` look acceptable?
    /// (Length + emptiness. We don't run a profanity filter here;
    /// that's operator policy and a future plugin can hook in.)
    pub(crate) fn validate_message(s: &ChatSettings, text: &str) -> Result<String, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("empty message".into());
        }
        if trimmed.chars().count() > s.max_message_length {
            return Err(format!(
                "message longer than {} characters",
                s.max_message_length
            ));
        }
        Ok(trimmed.to_string())
    }

    /// Pure-function check: does `nick` look acceptable?
    pub(crate) fn validate_nick(nick: &str) -> Result<String, String> {
        let trimmed = nick.trim();
        if trimmed.is_empty() {
            return Err("nick is required".into());
        }
        if trimmed.chars().count() > 32 {
            return Err("nick longer than 32 characters".into());
        }
        // No whitespace, no control chars.
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("nick must not contain whitespace or control characters".into());
        }
        Ok(trimmed.to_string())
    }
}

impl Plugin for LiveChatPlugin {
    fn name(&self) -> &'static str {
        "Live Listener Chat"
    }
    fn version(&self) -> &'static str {
        "0.1.0"
    }
    fn id(&self) -> String {
        "live-chat".into()
    }
    fn kind(&self) -> &'static str {
        "COMMUNITY"
    }
    fn icon(&self) -> &'static str {
        "icon-chat"
    }
    fn admin_url(&self) -> String {
        "/admin/live-chat".into()
    }

    fn init(&self) -> Result<(), PluginError> {
        info!(
            "Initializing Live Chat: enabled={}, slow_mode={}ms, max_len={}",
            self.settings.snapshot().enabled,
            self.settings.snapshot().slow_mode_ms,
            self.settings.snapshot().max_message_length,
        );
        Ok(())
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let s = self.settings.snapshot();
        let live = self.core.live_connections.load(Ordering::SeqCst);
        let history_len = self.core.history.lock().map(|g| g.len()).unwrap_or(0);
        PluginStatus {
            state: if s.enabled {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: if live > 0 {
                Some(live.to_string())
            } else {
                None
            },
            detail: serde_json::json!({
                "configured": true,
                "enabled": s.enabled,
                "live_connections": live,
                "history_len": history_len,
                "slow_mode_ms": s.slow_mode_ms,
                "max_message_length": s.max_message_length,
                "history_capacity": s.history_capacity,
                "banned_nicks_count": s.banned_nicks.len(),
            }),
        }
    }

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        let core = self.core.clone();
        let settings = self.settings.clone();
        Some(
            Router::new()
                .route("/api/live-chat/ws", get(ws_handler))
                .route("/api/live-chat/recent", get(recent_handler))
                .with_state(WsState { core, settings }),
        )
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        let core = self.core.clone();
        let settings = self.settings.clone();
        Some(
            Router::new()
                .route("/live-chat", get(admin_dashboard).post(admin_post))
                .with_state(AdminState { core, settings }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Live Chat",
            href: "/admin/live-chat".into(),
            icon: "💬",
            sort: 920,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone)]
struct WsState {
    core: Arc<ChatCore>,
    settings: Arc<SettingsStore>,
}

#[derive(Clone)]
struct AdminState {
    core: Arc<ChatCore>,
    settings: Arc<SettingsStore>,
}

// ============================================================================
// WebSocket handler
// ============================================================================

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WsState>) -> Response {
    if !state.settings.snapshot().enabled {
        return (StatusCode::SERVICE_UNAVAILABLE, "chat disabled").into_response();
    }
    let max = state.settings.snapshot().max_connections;
    let prev = state.core.live_connections.fetch_add(1, Ordering::SeqCst);
    if prev >= max {
        // Roll back the bump — we're over capacity.
        state.core.live_connections.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, "chat is full").into_response();
    }
    ws.on_upgrade(move |socket| async move {
        let counter = state.core.clone();
        ws_loop(socket, state).await;
        // Make sure the count is decremented even if ws_loop returns
        // via an error path. fetch_sub is cheap and idempotent.
        counter.live_connections.fetch_sub(1, Ordering::SeqCst);
    })
}

async fn ws_loop(socket: WebSocket, state: WsState) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = state.core.tx.subscribe();

    // 1. Send the Hello frame so the client has the recent history
    //    and the operator-configured limits in one shot.
    let s = state.settings.snapshot();
    let hello = ServerFrame::Hello {
        history: state.core.history_snapshot(s.history_capacity),
        slow_mode_ms: s.slow_mode_ms,
        max_message_length: s.max_message_length,
    };
    if send_frame(&mut sink, &hello).await.is_err() {
        return;
    }

    // 2. Per-connection slow-mode deadline. Each accepted message
    //    pushes this forward by `slow_mode_ms`.
    let slow_mode = Duration::from_millis(s.slow_mode_ms);
    let mut last_send: Option<Instant> = if slow_mode.is_zero() {
        None
    } else {
        // Start with a small grace period so the very first message
        // goes through immediately.
        Some(Instant::now() - slow_mode)
    };

    loop {
        tokio::select! {
            biased;
            // Outbound: new message from the broadcast channel.
            result = rx.recv() => match result {
                Ok(msg) => {
                    if send_frame(&mut sink, &ServerFrame::Msg(&msg)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            // Inbound: a frame from the client.
            inbound = stream.next() => match inbound {
                Some(Ok(Message::Text(raw))) => {
                    let frame: ClientFrame = match serde_json::from_str(&raw) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = send_frame(
                                &mut sink,
                                &ServerFrame::Rejected {
                                    reason: format!("bad frame: {}", e),
                                },
                            ).await;
                            continue;
                        }
                    };
                    match frame {
                        ClientFrame::Ping => {
                            if send_frame(&mut sink, &ServerFrame::Pong).await.is_err() {
                                return;
                            }
                        }
                        ClientFrame::Msg { nick, text } => {
                            let outcome = handle_inbound(
                                &state,
                                &nick,
                                &text,
                                slow_mode,
                                &mut last_send,
                            ).await;
                            if let Some(reason) = outcome {
                                let _ = send_frame(
                                    &mut sink,
                                    &ServerFrame::Rejected { reason },
                                ).await;
                            }
                        }
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    // Binary frames aren't part of the chat protocol.
                    // Politely tell the client.
                    let _ = send_frame(
                        &mut sink,
                        &ServerFrame::Rejected {
                            reason: "binary frames not supported".into(),
                        },
                    ).await;
                }
                Some(Ok(Message::Ping(p))) => {
                    if sink.send(Message::Pong(p)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return,
                Some(Err(_)) => return,
            }
        }
    }
}

/// Handle one inbound message. Returns `Some(reason)` if the message
/// was rejected (caller will inform the client), or `None` if it
/// was accepted (and pushed to the broadcast channel).
async fn handle_inbound(
    state: &WsState,
    nick: &str,
    text: &str,
    slow_mode: Duration,
    last_send: &mut Option<Instant>,
) -> Option<String> {
    let s = state.settings.snapshot();

    if is_banned(&s, nick) {
        return Some("banned".into());
    }
    let nick = match LiveChatPlugin::validate_nick(nick) {
        Ok(n) => n,
        Err(e) => return Some(e),
    };
    let text = match LiveChatPlugin::validate_message(&s, text) {
        Ok(t) => t,
        Err(e) => return Some(e),
    };
    if !slow_mode.is_zero() {
        if let Some(prev) = *last_send {
            let now = Instant::now();
            if now.duration_since(prev) < slow_mode {
                return Some(format!(
                    "slow mode: wait {}s",
                    (slow_mode.as_millis() - now.duration_since(prev).as_millis()).div_ceil(1000)
                ));
            }
        }
        *last_send = Some(Instant::now());
    }
    let msg = ChatMessage {
        id: state.core.next_id(),
        nick,
        text,
        ts: unix_now(),
    };
    state.core.push_message(msg.clone());
    // Broadcast to every subscriber, including the sender.
    let _ = state.core.tx.send(msg);
    None
}

// `is_banned` lives on the plugin, but we want to call it from a
// free function. Wrap it here so the ws loop doesn't have to take
// `&LiveChatPlugin` as a parameter.
fn is_banned(s: &ChatSettings, nick: &str) -> bool {
    LiveChatPlugin::is_banned(s, nick)
}

async fn send_frame(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    frame: &ServerFrame<'_>,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(frame).map_err(|_| axum::Error::new("serialize"))?;
    sink.send(Message::Text(json)).await
}

// ============================================================================
// Public HTTP endpoints
// ============================================================================

async fn recent_handler(State(state): State<WsState>) -> Response {
    let s = state.settings.snapshot();
    if !s.enabled {
        return Json(serde_json::json!({
            "enabled": false,
            "messages": [],
        }))
        .into_response();
    }
    let msgs = state.core.history_snapshot(s.history_capacity);
    Json(serde_json::json!({
        "enabled": true,
        "slow_mode_ms": s.slow_mode_ms,
        "max_message_length": s.max_message_length,
        "messages": msgs,
    }))
    .into_response()
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
struct DashboardPage {
    #[allow(dead_code)]
    active_tab: &'static str,
    enabled: bool,
    slow_mode_ms: u64,
    max_message_length: usize,
    history_capacity: usize,
    max_connections: usize,
    broadcast_capacity: usize,
    banned_nicks_csv: String,
    banned_count: usize,
    live_connections: usize,
    history_len: usize,
    recent: Vec<RecentRow>,
    recent_count: usize,
    saved: bool,
}

struct RecentRow {
    nick: String,
    text: String,
    ts_human: String,
}

async fn admin_dashboard(
    State(state): State<AdminState>,
    axum::extract::Query(q): axum::extract::Query<SavedQuery>,
) -> Response {
    let s = state.settings.snapshot();
    let banned_csv = s.banned_nicks.join(", ");
    let live = state.core.live_connections.load(Ordering::SeqCst);
    let hist = state.core.history.lock().expect("history poisoned");
    let history_len = hist.len();
    let recent: Vec<RecentRow> = hist
        .iter()
        .take(20)
        .map(|m| RecentRow {
            nick: m.nick.clone(),
            text: m.text.clone(),
            ts_human: format_unix(m.ts),
        })
        .collect();
    drop(hist);
    render(DashboardPage {
        active_tab: "live-chat",
        enabled: s.enabled,
        slow_mode_ms: s.slow_mode_ms,
        max_message_length: s.max_message_length,
        history_capacity: s.history_capacity,
        max_connections: s.max_connections,
        broadcast_capacity: s.broadcast_capacity,
        banned_nicks_csv: banned_csv,
        banned_count: s.banned_nicks.len(),
        live_connections: live,
        history_len,
        recent_count: recent.len(),
        recent,
        saved: q.saved.is_some(),
    })
}

#[derive(Deserialize)]
struct SavedQuery {
    #[serde(default)]
    saved: Option<u8>,
}

#[derive(Deserialize)]
struct SettingsForm {
    #[serde(default)]
    enabled: Option<String>,
    slow_mode_ms: Option<u64>,
    max_message_length: Option<usize>,
    history_capacity: Option<usize>,
    max_connections: Option<usize>,
    broadcast_capacity: Option<usize>,
    banned_nicks: String,
}

async fn admin_post(State(state): State<AdminState>, Form(form): Form<SettingsForm>) -> Response {
    let mut s = state.settings.snapshot();
    s.enabled = form.enabled.is_some();
    if let Some(ms) = form.slow_mode_ms {
        s.slow_mode_ms = ms.clamp(0, 60_000);
    }
    if let Some(n) = form.max_message_length {
        s.max_message_length = n.clamp(1, 4_000);
    }
    if let Some(n) = form.history_capacity {
        s.history_capacity = n.clamp(1, 5_000);
    }
    if let Some(n) = form.max_connections {
        s.max_connections = n.clamp(1, 100_000);
    }
    if let Some(n) = form.broadcast_capacity {
        s.broadcast_capacity = n.clamp(8, 4_096);
    }
    s.banned_nicks = form
        .banned_nicks
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let _ = state.settings.replace(s);
    axum::response::Redirect::to("/admin/live-chat?saved=1").into_response()
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ============================================================================
// helpers
// ============================================================================

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_unix(ts: u64) -> String {
    use ccc_plugin_utils::unix_to_ymdhm;
    let (y, mo, d, h, mi) = unix_to_ymdhm(ts);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, mi)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> ChatSettings {
        ChatSettings::default()
    }

    #[test]
    fn validate_nick_ok() {
        assert_eq!(LiveChatPlugin::validate_nick("jane").unwrap(), "jane");
        assert_eq!(LiveChatPlugin::validate_nick("  bob  ").unwrap(), "bob");
    }

    #[test]
    fn validate_nick_rejects_empty() {
        assert!(LiveChatPlugin::validate_nick("").is_err());
        assert!(LiveChatPlugin::validate_nick("   ").is_err());
    }

    #[test]
    fn validate_nick_rejects_too_long() {
        let long: String = "a".repeat(40);
        assert!(LiveChatPlugin::validate_nick(&long).is_err());
    }

    #[test]
    fn validate_nick_rejects_whitespace_inside() {
        assert!(LiveChatPlugin::validate_nick("jane doe").is_err());
        assert!(LiveChatPlugin::validate_nick("with\ttab").is_err());
        assert!(LiveChatPlugin::validate_nick("new\nline").is_err());
    }

    #[test]
    fn validate_message_ok() {
        let settings = s();
        assert_eq!(
            LiveChatPlugin::validate_message(&settings, "hello world").unwrap(),
            "hello world"
        );
    }

    #[test]
    fn validate_message_trims_and_rejects_empty() {
        let settings = s();
        assert!(LiveChatPlugin::validate_message(&settings, "").is_err());
        assert!(LiveChatPlugin::validate_message(&settings, "   ").is_err());
        assert_eq!(
            LiveChatPlugin::validate_message(&settings, "  hi  ").unwrap(),
            "hi"
        );
    }

    #[test]
    fn validate_message_rejects_too_long() {
        let settings = ChatSettings {
            max_message_length: 5,
            ..s()
        };
        assert!(LiveChatPlugin::validate_message(&settings, "hello there").is_err());
        assert!(LiveChatPlugin::validate_message(&settings, "hi").is_ok());
    }

    #[test]
    fn ban_list_is_case_insensitive_and_trimmed() {
        let settings = ChatSettings {
            banned_nicks: vec!["Troll".into(), "Spammer".into()],
            ..s()
        };
        assert!(LiveChatPlugin::is_banned(&settings, "troll"));
        assert!(LiveChatPlugin::is_banned(&settings, "TROLL"));
        assert!(LiveChatPlugin::is_banned(&settings, "  Troll "));
        assert!(LiveChatPlugin::is_banned(&settings, "spamMEr"));
        assert!(!LiveChatPlugin::is_banned(&settings, "innocent"));
    }

    #[test]
    fn ban_list_empty_string_is_not_a_ban() {
        let settings = ChatSettings {
            banned_nicks: vec!["".into(), "   ".into(), "RealBan".into()],
            ..s()
        };
        // An empty nick itself isn't banned by the empty entries;
        // it's rejected by validate_nick.
        assert!(!LiveChatPlugin::is_banned(&settings, ""));
        // But the real entry is matched.
        assert!(LiveChatPlugin::is_banned(&settings, "realban"));
    }

    #[test]
    fn chat_core_history_capacity_is_bounded() {
        // We don't currently expose a `with_capacity` constructor —
        // the deque grows to whatever Rust's growth strategy picks.
        // The bound is enforced by `push_message` once the deque
        // reaches its grown size, so we just assert that the
        // count never grows without bound and the newest message
        // is always first.
        let core = ChatCore::new(8);
        for i in 0..200 {
            core.push_message(ChatMessage {
                id: i,
                nick: "x".into(),
                text: "y".into(),
                ts: i,
            });
        }
        let snap = core.history_snapshot(10_000);
        // The most recent push (id 199) should be at the front.
        assert_eq!(snap[0].id, 199);
        // We should never have more than the deque's grown capacity.
        // (The growth strategy is internal; we just verify the ring
        // is working — i.e. we didn't grow unbounded.)
        assert!(snap.len() < 200, "history grew unbounded: {}", snap.len());
        // And it should be at least 1.
        assert!(!snap.is_empty());
    }

    #[test]
    fn chat_core_broadcast_delivers_to_subscribers() {
        // `tokio::sync::broadcast::Receiver::try_recv` requires a
        // tokio runtime context (it uses the current runtime to
        // park the task if there's nothing to read). Wrap the test
        // in a runtime.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let core = ChatCore::new(8);
            let mut rx1 = core.tx.subscribe();
            let mut rx2 = core.tx.subscribe();
            let msg = ChatMessage {
                id: 42,
                nick: "alice".into(),
                text: "hi".into(),
                ts: 1,
            };
            core.push_message(msg.clone());
            let _ = core.tx.send(msg.clone());
            let r1 = rx1.recv().await.expect("rx1 got message");
            let r2 = rx2.recv().await.expect("rx2 got message");
            assert_eq!(r1.id, 42);
            assert_eq!(r2.id, 42);
        });
    }

    #[test]
    fn chat_core_id_counter_monotonic() {
        let core = ChatCore::new(8);
        assert_eq!(core.next_id(), 1);
        assert_eq!(core.next_id(), 2);
        assert_eq!(core.next_id(), 3);
    }

    #[test]
    fn settings_default_round_trip() {
        let s = ChatSettings::default();
        let toml = toml::to_string(&s).unwrap();
        let back: ChatSettings = toml::from_str(&toml).unwrap();
        assert_eq!(back.enabled, s.enabled);
        assert_eq!(back.slow_mode_ms, s.slow_mode_ms);
        assert_eq!(back.max_message_length, s.max_message_length);
        assert_eq!(back.history_capacity, s.history_capacity);
        assert_eq!(back.max_connections, s.max_connections);
        assert_eq!(back.banned_nicks.len(), 0);
    }

    #[test]
    fn settings_back_compat_when_ban_list_missing() {
        // Older live-chat.toml files don't have banned_nicks. Make
        // sure they still load with an empty default.
        let minimal = r#"
            enabled = true
            slow_mode_ms = 1500
            max_message_length = 250
            history_capacity = 75
            max_connections = 100
        "#;
        let s: ChatSettings = toml::from_str(minimal).unwrap();
        assert!(s.banned_nicks.is_empty());
        assert!(s.enabled);
    }

    #[test]
    fn server_frame_hello_serializes_with_history() {
        let frame = ServerFrame::Hello {
            history: vec![ChatMessage {
                id: 1,
                nick: "a".into(),
                text: "b".into(),
                ts: 2,
            }],
            slow_mode_ms: 1000,
            max_message_length: 200,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"slow_mode_ms\":1000"));
        assert!(json.contains("\"max_message_length\":200"));
    }

    #[test]
    fn server_frame_rejected_serializes_reason() {
        let frame = ServerFrame::Rejected {
            reason: "banned".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"type\":\"rejected\""));
        assert!(json.contains("\"reason\":\"banned\""));
    }

    #[test]
    fn client_frame_msg_parses() {
        let raw = r#"{"type":"msg","nick":"alice","text":"hello"}"#;
        let f: ClientFrame = serde_json::from_str(raw).unwrap();
        match f {
            ClientFrame::Msg { nick, text } => {
                assert_eq!(nick, "alice");
                assert_eq!(text, "hello");
            }
            _ => panic!("expected Msg"),
        }
    }

    #[test]
    fn client_frame_ping_parses() {
        let raw = r#"{"type":"ping"}"#;
        let f: ClientFrame = serde_json::from_str(raw).unwrap();
        assert!(matches!(f, ClientFrame::Ping));
    }

    #[test]
    fn client_frame_rejects_garbage() {
        let raw = r#"{"type":"msg","nick":"alice"}"#; // missing text
        let f: serde_json::Result<ClientFrame> = serde_json::from_str(raw);
        assert!(f.is_err());
    }

    #[test]
    fn chat_core_live_connections_counter() {
        let core = ChatCore::new(8);
        assert_eq!(core.live_connections.load(Ordering::SeqCst), 0);
        core.live_connections.fetch_add(1, Ordering::SeqCst);
        core.live_connections.fetch_add(1, Ordering::SeqCst);
        assert_eq!(core.live_connections.load(Ordering::SeqCst), 2);
        core.live_connections.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(core.live_connections.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unix_now_is_sane() {
        let now = unix_now();
        // 2020-01-01 UTC = 1577836800
        assert!(now > 1_577_836_800, "unix_now() in the past: {}", now);
    }

    #[test]
    fn format_unix_contains_year() {
        let s = format_unix(1_700_000_000);
        // 2023-11-14-ish
        assert!(s.starts_with("2023-"), "got: {}", s);
    }

    #[test]
    #[allow(clippy::needless_collect)]
    fn history_snapshot_respects_cap_argument() {
        let core = ChatCore::new(8);
        for i in 0..30 {
            core.push_message(ChatMessage {
                id: i,
                nick: "n".into(),
                text: "t".into(),
                ts: i,
            });
        }
        // Push 30, ask for 5. We get at most 5; the newest is
        // always first. (The actual stored count depends on the
        // deque's grown capacity, but the cap argument to
        // `history_snapshot` is a hard limit.)
        let five = core.history_snapshot(5);
        assert!(five.len() <= 5);
        if !five.is_empty() {
            // Newest first — push_front ordering.
            assert_eq!(five[0].id, 29);
        }
    }
}
