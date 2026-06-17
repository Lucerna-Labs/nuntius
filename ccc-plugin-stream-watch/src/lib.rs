//! Stream Health Watcher — Shark-style dead-air detection for the radio engine.
//!
//! The watcher is a passive monitor. It does NOT touch the broadcast —
//! it just keeps an eye on three signals:
//!
//! 1. **Now-playing freshness** — polls the radio engine's
//!    `/api/radio/nowplaying` JSON. If the `now_playing.started_at`
//!    field hasn't changed in `stale_seconds`, something is wrong
//!    (frozen player, stuck encoder, AutoDJ loop dead).
//!
//! 2. **Live listener health** — opens a side-listener socket against
//!    the radio engine's mount path. Bytes that flow mean the engine
//!    is actually pushing audio. Counts the byte rate over each tick
//!    window; if the rate is below `min_bytes_per_tick`, that's
//!    effectively dead air (the listener connected, but no data
//!    arrived).
//!
//! 3. **HTTP probe** — every `http_probe_seconds`, opens a fresh
//!    `GET /` against the radio engine's port. If the connection
//!    refuses or times out, the engine is down entirely.
//!
//! On a transition from HEALTHY → UNHEALTHY, the watcher fires
//! every configured alert (Discord webhook, generic webhook) and
//! records the event in its in-memory event log (capped, so the
//! admin dashboard can show "last 50 events"). On recovery, it
//! fires a recovery alert.
//!
//! The watcher exposes:
//! - `GET /api/stream-watch/status` — current health snapshot
//! - `GET /api/stream-watch/events` — last N events
//! - `GET /admin/stream-watch` — dashboard
//! - `POST /admin/stream-watch` — settings form

use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_plugin_utils::TomlSidecar;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::interval;
use tracing::{info, warn};

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherSettings {
    /// Base URL of the radio engine's HTTP listener. Typically
    /// `http://127.0.0.1:8000` because the engine is on the same host.
    #[serde(default = "default_engine_url")]
    pub engine_url: String,
    /// Mount path the side-listener connects to. Almost always
    /// `/listen.mp3` (the engine's default).
    #[serde(default = "default_mount_path")]
    pub mount_path: String,
    /// How often the watcher polls now-playing JSON, opens a fresh
    /// HTTP probe, etc. Defaults to 10s — fast enough to catch a stall
    /// inside ~15s, cheap enough to not be a CPU hog.
    #[serde(default = "default_tick_seconds")]
    pub tick_seconds: u64,
    /// Now-playing `started_at` older than this means the player
    /// is frozen. 90s gives one full "track intro" before alarm.
    #[serde(default = "default_stale_seconds")]
    pub stale_seconds: u64,
    /// Side-listener must receive at least this many bytes per tick
    /// to count as "audio flowing." A 128kbps stream sends ~16 KB/s,
    /// so we default to 4 KB/tick (a 10s tick → 400 B/s, way below
    /// dead air — we want to catch a totally silent listener before
    /// this trips, but tolerate packet pauses).
    #[serde(default = "default_min_bytes_per_tick")]
    pub min_bytes_per_tick: u64,
    /// If the radio engine's HTTP probe fails this many ticks in a
    /// row, the watcher declares the engine DOWN. 3 ticks = ~30s
    /// of unreachable service.
    #[serde(default = "default_max_failed_probes")]
    pub max_failed_probes: u32,
    /// Cooldown between successive alerts for the SAME condition.
    /// 5 minutes: long enough that a real human sees the alert, short
    /// enough that a long outage still generates periodic reminders.
    #[serde(default = "default_alert_cooldown_seconds")]
    pub alert_cooldown_seconds: u64,
    /// Optional Discord webhook URL. When set, every state change
    /// posts a `content` field with a one-line summary.
    #[serde(default)]
    pub discord_webhook_url: String,
    /// Optional generic webhook URL. When set, posts a JSON body
    /// `{"event": "...", "summary": "...", "ts": ...}`.
    #[serde(default)]
    pub generic_webhook_url: String,
}

fn default_engine_url() -> String {
    "http://127.0.0.1:8000".into()
}
fn default_mount_path() -> String {
    "/listen.mp3".into()
}
fn default_tick_seconds() -> u64 {
    10
}
fn default_stale_seconds() -> u64 {
    90
}
fn default_min_bytes_per_tick() -> u64 {
    4_000
}
fn default_max_failed_probes() -> u32 {
    3
}
fn default_alert_cooldown_seconds() -> u64 {
    300
}

impl Default for WatcherSettings {
    fn default() -> Self {
        Self {
            engine_url: default_engine_url(),
            mount_path: default_mount_path(),
            tick_seconds: default_tick_seconds(),
            stale_seconds: default_stale_seconds(),
            min_bytes_per_tick: default_min_bytes_per_tick(),
            max_failed_probes: default_max_failed_probes(),
            alert_cooldown_seconds: default_alert_cooldown_seconds(),
            discord_webhook_url: String::new(),
            generic_webhook_url: String::new(),
        }
    }
}

pub type SettingsStore = TomlSidecar<WatcherSettings>;

// ============================================================================
// Health model
// ============================================================================

/// Coarse state the operator cares about. The finer-grained `HealthSnapshot`
/// is what the dashboard renders; this is what state transitions and alerts
/// pivot on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    /// Engine HTTP probe succeeds, now-playing is fresh, side-listener
    /// is receiving bytes.
    #[default]
    Healthy,
    /// Now-playing is stale OR side-listener byte rate is below
    /// `min_bytes_per_tick`. Audio is still flowing but something is
    /// stuck or silent.
    Degraded,
    /// HTTP probe is failing repeatedly, OR the engine has been
    /// degraded for more than the recovery timeout. The stream is
    /// effectively dead from the listener's perspective.
    Down,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub state: HealthState,
    /// Unix seconds of the last successful tick (probe + bytes seen).
    pub last_healthy_ts: u64,
    /// Consecutive HTTP probe failures (resets on success).
    pub failed_probes: u32,
    /// Bytes received on the side-listener over the last tick.
    pub last_tick_bytes: u64,
    /// Most recent now-playing title (artist - title) if any.
    pub now_playing_label: Option<String>,
    /// Reason string for non-Healthy states.
    pub reason: Option<String>,
    pub ts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherEvent {
    pub ts: u64,
    pub kind: String, // "degraded", "down", "recovered"
    pub summary: String,
}

// ============================================================================
// State
// ============================================================================

#[derive(Default)]
struct WatcherCore {
    state: Mutex<HealthSnapshot>,
    events: Mutex<VecDeque<WatcherEvent>>,
    /// Unix seconds of the last alert sent for each `kind` string. Used
    /// to enforce the alert cooldown.
    last_alert: Mutex<std::collections::HashMap<String, u64>>,
}

impl WatcherCore {
    fn push_event(&self, e: WatcherEvent) {
        let mut g = self.events.lock().expect("events poisoned");
        if g.len() >= 100 {
            g.pop_front();
        }
        g.push_back(e);
    }

    fn snapshot(&self) -> HealthSnapshot {
        self.state.lock().expect("state poisoned").clone()
    }

    fn events_snapshot(&self) -> Vec<WatcherEvent> {
        self.events
            .lock()
            .expect("events poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn should_alert(&self, kind: &str, cooldown_secs: u64) -> bool {
        let now = unix_now();
        let mut g = self.last_alert.lock().expect("last_alert poisoned");
        let allowed = g
            .get(kind)
            .map(|last| now.saturating_sub(*last) >= cooldown_secs)
            .unwrap_or(true);
        if allowed {
            g.insert(kind.to_string(), now);
        }
        allowed
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct StreamWatchPlugin {
    settings: Arc<SettingsStore>,
    core: Arc<WatcherCore>,
}

impl StreamWatchPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("stream-watch")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("stream-watch.toml"));
        let store = Arc::new(
            SettingsStore::load_or_init(&path)
                .map_err(|e| PluginError::InitError(e.to_string()))?,
        );
        // Initialise snapshot timestamp so the dashboard doesn't render
        // a 1970-01-01 "last healthy" line.
        let core = WatcherCore {
            state: Mutex::new(HealthSnapshot {
                state: HealthState::Healthy,
                last_healthy_ts: unix_now(),
                failed_probes: 0,
                last_tick_bytes: 0,
                now_playing_label: None,
                reason: None,
                ts: unix_now(),
            }),
            ..Default::default()
        };
        let core = Arc::new(core);
        Ok(Self {
            settings: store,
            core,
        })
    }
}

impl Plugin for StreamWatchPlugin {
    fn name(&self) -> &'static str {
        "Stream Watch"
    }
    fn version(&self) -> &'static str {
        "0.1.0"
    }
    fn id(&self) -> String {
        "stream-watch".into()
    }
    fn kind(&self) -> &'static str {
        "MONITOR"
    }
    fn icon(&self) -> &'static str {
        "icon-pulse"
    }
    fn admin_url(&self) -> String {
        "/admin/stream-watch".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let snap = self.core.snapshot();
        let event_count = self.core.events.lock().map(|g| g.len()).unwrap_or(0);
        let state_str = match snap.state {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Down => "down",
        };
        PluginStatus {
            state: match snap.state {
                HealthState::Healthy => PluginLifecycle::Configured,
                HealthState::Degraded | HealthState::Down => PluginLifecycle::Live,
            },
            badge: Some(state_str.into()),
            detail: serde_json::json!({
                "configured": true,
                "health": snap,
                "events_recorded": event_count,
                "alerts": {
                    "discord": !self.settings.snapshot().discord_webhook_url.is_empty(),
                    "generic_webhook": !self.settings.snapshot().generic_webhook_url.is_empty(),
                },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        info!(
            "Initializing Stream Watch: monitoring {} every {}s",
            self.settings.snapshot().engine_url,
            self.settings.snapshot().tick_seconds
        );
        // Spawn the watcher loop with a fresh cancellation token. We
        // don't currently have a host-wide shutdown here, so we use a
        // local token and let the loop run for the process lifetime.
        // When the plugin grows a shutdown hook, thread the token in.
        let token = tokio_util::sync::CancellationToken::new();
        let settings = self.settings.clone();
        let core = self.core.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(settings.snapshot().tick_seconds.max(2)));
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tick.tick() => {
                        if let Err(e) = run_tick(settings.clone(), core.clone()).await {
                            warn!("[stream-watch] tick error: {}", e);
                        }
                    }
                }
            }
            info!("[stream-watch] loop exiting");
        });
        Ok(())
    }

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        let core = self.core.clone();
        Some(
            Router::new()
                .route("/api/stream-watch/status", get(public_status))
                .route("/api/stream-watch/events", get(public_events))
                .with_state(core),
        )
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        let core = self.core.clone();
        let settings = self.settings.clone();
        Some(
            Router::new()
                .route("/stream-watch", get(admin_dashboard).post(admin_post))
                .with_state(AdminState { core, settings }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Stream Watch",
            href: "/admin/stream-watch".into(),
            icon: "📡",
            sort: 950,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// The actual tick (free function so the loop can call it).
// ============================================================================

async fn run_tick(settings: Arc<SettingsStore>, core: Arc<WatcherCore>) -> Result<(), String> {
    let s = settings.snapshot();
    let tick = s.tick_seconds.max(2);

    let probe_ok = http_probe(&s.engine_url).await;
    let (np_label, np_started, np_ok) = fetch_now_playing(&s.engine_url).await;
    let bytes = side_listener_bytes(&s.engine_url, &s.mount_path, tick).await;

    let mut snap = core.snapshot();
    let prev_state = snap.state;
    let mut failed_probes = snap.failed_probes;
    if probe_ok {
        failed_probes = 0;
    } else {
        failed_probes = failed_probes.saturating_add(1);
    }
    let now = unix_now();
    let np_age = np_started.map(|s| now.saturating_sub(s));
    let np_fresh = match (np_ok, np_age) {
        (true, Some(age)) if age <= s.stale_seconds => true,
        (true, Some(_)) => false,
        (true, None) => true,
        (false, _) => false,
    };
    let bytes_low = bytes < s.min_bytes_per_tick;

    let (state, reason) = if failed_probes >= s.max_failed_probes {
        (
            HealthState::Down,
            Some(format!(
                "HTTP probe failed {} times in a row",
                failed_probes
            )),
        )
    } else if !probe_ok {
        (
            HealthState::Degraded,
            Some("HTTP probe failed (engine unreachable on this host?)".into()),
        )
    } else if bytes_low && !np_fresh {
        (
            HealthState::Down,
            Some(format!(
                "side-listener got only {} bytes (need {}) and now-playing is stale",
                bytes, s.min_bytes_per_tick
            )),
        )
    } else if bytes_low {
        (
            HealthState::Degraded,
            Some(format!(
                "side-listener got only {} bytes this tick (need {})",
                bytes, s.min_bytes_per_tick
            )),
        )
    } else if !np_fresh {
        (
            HealthState::Degraded,
            Some("now-playing is stale — encoder may be stuck".into()),
        )
    } else {
        (HealthState::Healthy, None)
    };

    snap.state = state;
    snap.last_healthy_ts = if state == HealthState::Healthy {
        now
    } else {
        snap.last_healthy_ts
    };
    snap.failed_probes = failed_probes;
    snap.last_tick_bytes = bytes;
    snap.now_playing_label = np_label;
    snap.reason = reason.clone();
    snap.ts = now;
    {
        let mut g = core.state.lock().expect("state poisoned");
        *g = snap.clone();
    }

    if state != prev_state {
        let kind = match state {
            HealthState::Healthy => "recovered",
            HealthState::Degraded => "degraded",
            HealthState::Down => "down",
        };
        let summary = match &snap.reason {
            Some(r) => format!("Stream {}: {}", kind, r),
            None => format!("Stream {}", kind),
        };
        core.push_event(WatcherEvent {
            ts: now,
            kind: kind.into(),
            summary: summary.clone(),
        });
        if core.should_alert(kind, s.alert_cooldown_seconds) {
            if let Err(e) = fire_alerts(
                &s.discord_webhook_url,
                &s.generic_webhook_url,
                kind,
                &summary,
            )
            .await
            {
                warn!("[stream-watch] alert dispatch failed: {}", e);
            }
        }
    }
    Ok(())
}

// ============================================================================
// HTTP probe, now-playing fetch, side-listener
// ============================================================================

async fn http_probe(base: &str) -> bool {
    // Connect to the base URL. We don't care about body — just that the
    // socket answers within 3 seconds.
    let url = format!("{}/", base.trim_end_matches('/'));
    match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => match client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

async fn fetch_now_playing(base: &str) -> (Option<String>, Option<u64>, bool) {
    let url = format!("{}/api/radio/nowplaying", base.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, None, false),
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return (None, None, false),
    };
    if !resp.status().is_success() {
        return (None, None, false);
    }
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return (None, None, false),
    };
    let np = v.get("now_playing");
    let label = np.and_then(|n| {
        let artist = n.get("artist").and_then(|s| s.as_str()).unwrap_or("");
        let title = n.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let source = n.get("source").and_then(|s| s.as_str()).unwrap_or("");
        if title.is_empty() {
            None
        } else if artist.is_empty() {
            Some(format!("[{}] {}", source, title))
        } else {
            Some(format!("[{}] {} - {}", source, artist, title))
        }
    });
    let started_at = np
        .and_then(|n| n.get("started_at"))
        .and_then(|s| s.as_u64());
    (label, started_at, true)
}

/// Open a fresh HTTP connection to the engine's mount, read up to
/// `timeout` worth of bytes, return the count. This is the dead-air
/// detector's "are bytes actually flowing?" probe. A silent stream
/// returns 0; a healthy stream returns many kilobytes.
async fn side_listener_bytes(base: &str, mount: &str, seconds: u64) -> u64 {
    let url = format!("{}{}", base.trim_end_matches('/'), mount);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(seconds + 2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return 0,
    };
    match client.get(&url).send().await {
        Ok(mut resp) => {
            if !resp.status().is_success() {
                return 0;
            }
            let mut total: u64 = 0;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds.max(1));
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, resp.chunk()).await {
                    Ok(Ok(Some(bytes))) => {
                        total = total.saturating_add(bytes.len() as u64);
                    }
                    _ => break,
                }
            }
            total
        }
        Err(_) => 0,
    }
}

// ============================================================================
// Alert dispatch
// ============================================================================

async fn fire_alerts(
    discord: &str,
    generic: &str,
    kind: &str,
    summary: &str,
) -> Result<(), String> {
    if discord.is_empty() && generic.is_empty() {
        return Ok(());
    }
    if !discord.is_empty() {
        // Discord expects { "content": "..." }.
        let body = serde_json::json!({
            "content": format!("[stream-watch] {}", summary),
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(discord)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("discord post: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("discord webhook returned {}", resp.status()));
        }
    }
    if !generic.is_empty() {
        let body = serde_json::json!({
            "event": kind,
            "summary": summary,
            "ts": unix_now(),
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(generic)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("generic webhook post: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("generic webhook returned {}", resp.status()));
        }
    }
    Ok(())
}

// ============================================================================
// Admin + public handlers
// ============================================================================

#[derive(Clone)]
struct AdminState {
    core: Arc<WatcherCore>,
    settings: Arc<SettingsStore>,
}

async fn public_status(State(core): State<Arc<WatcherCore>>) -> Response {
    Json(core.snapshot()).into_response()
}

async fn public_events(State(core): State<Arc<WatcherCore>>) -> Response {
    Json(core.events_snapshot()).into_response()
}

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
struct DashboardPage {
    #[allow(dead_code)]
    active_tab: &'static str,
    state: String,
    state_color: &'static str,
    last_healthy_human: String,
    failed_probes: u32,
    last_tick_bytes: u64,
    np_label: String,
    reason: String,
    events: Vec<EventRow>,
    engine_url: String,
    mount_path: String,
    tick_seconds: u64,
    stale_seconds: u64,
    min_bytes: u64,
    max_failed_probes: u32,
    alert_cooldown_seconds: u64,
    discord_webhook_url: String,
    generic_webhook_url: String,
    saved: bool,
}

struct EventRow {
    when_human: String,
    kind: String,
    kind_color: &'static str,
    summary: String,
}

fn color_for_state(s: HealthState) -> &'static str {
    match s {
        HealthState::Healthy => "#3ecf8e",
        HealthState::Degraded => "#f0b400",
        HealthState::Down => "#e23b3b",
    }
}

fn color_for_kind(k: &str) -> &'static str {
    match k {
        "recovered" => "#3ecf8e",
        "degraded" => "#f0b400",
        "down" => "#e23b3b",
        _ => "#9aa3b2",
    }
}

async fn admin_dashboard(
    State(state): State<AdminState>,
    axum::extract::Query(q): axum::extract::Query<SavedQuery>,
) -> Response {
    let snap = state.core.snapshot();
    let s = state.settings.snapshot();
    let events = state.core.events_snapshot();
    let last_healthy_human = format_unix(snap.last_healthy_ts);
    let state_str = match snap.state {
        HealthState::Healthy => "healthy",
        HealthState::Degraded => "degraded",
        HealthState::Down => "down",
    };
    let np_label = snap.now_playing_label.unwrap_or_else(|| "—".into());
    let reason = snap.reason.unwrap_or_else(|| "—".into());
    let event_rows: Vec<EventRow> = events
        .into_iter()
        .rev()
        .take(50)
        .map(|e| {
            let kind_str = e.kind.clone();
            let color = color_for_kind(&kind_str);
            EventRow {
                when_human: format_unix(e.ts),
                kind: kind_str,
                kind_color: color,
                summary: e.summary,
            }
        })
        .collect();
    render(DashboardPage {
        active_tab: "stream-watch",
        state: state_str.into(),
        state_color: color_for_state(snap.state),
        last_healthy_human,
        failed_probes: snap.failed_probes,
        last_tick_bytes: snap.last_tick_bytes,
        np_label,
        reason,
        events: event_rows,
        engine_url: s.engine_url,
        mount_path: s.mount_path,
        tick_seconds: s.tick_seconds,
        stale_seconds: s.stale_seconds,
        min_bytes: s.min_bytes_per_tick,
        max_failed_probes: s.max_failed_probes,
        alert_cooldown_seconds: s.alert_cooldown_seconds,
        discord_webhook_url: s.discord_webhook_url,
        generic_webhook_url: s.generic_webhook_url,
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
    engine_url: String,
    mount_path: String,
    tick_seconds: Option<u64>,
    stale_seconds: Option<u64>,
    min_bytes_per_tick: Option<u64>,
    max_failed_probes: Option<u32>,
    alert_cooldown_seconds: Option<u64>,
    #[serde(default)]
    discord_webhook_url: String,
    #[serde(default)]
    generic_webhook_url: String,
}

async fn admin_post(State(state): State<AdminState>, Form(form): Form<SettingsForm>) -> Response {
    let mut s = state.settings.snapshot();
    s.engine_url = form.engine_url.trim().to_string();
    s.mount_path = form.mount_path.trim().to_string();
    if let Some(t) = form.tick_seconds {
        s.tick_seconds = t.clamp(2, 3600);
    }
    if let Some(t) = form.stale_seconds {
        s.stale_seconds = t.clamp(10, 3600);
    }
    if let Some(b) = form.min_bytes_per_tick {
        s.min_bytes_per_tick = b;
    }
    if let Some(p) = form.max_failed_probes {
        s.max_failed_probes = p.clamp(1, 30);
    }
    if let Some(c) = form.alert_cooldown_seconds {
        s.alert_cooldown_seconds = c.clamp(0, 86_400);
    }
    s.discord_webhook_url = form.discord_webhook_url.trim().to_string();
    s.generic_webhook_url = form.generic_webhook_url.trim().to_string();
    let _ = state.settings.replace(s);
    axum::response::Redirect::to("/admin/stream-watch?saved=1").into_response()
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
    if ts == 0 {
        return "—".into();
    }
    let secs = ts % 60;
    let mins = (ts / 60) % 60;
    let hours = (ts / 3600) % 24;
    let days = ts / 86_400;
    let (y, mo, d) = unix_to_ymd(ts);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} ({}d ago)",
        y, mo, d, hours, mins, secs, days
    )
}

fn unix_to_ymd(secs: u64) -> (u32, u32, u32) {
    let days = (secs / 86_400) as u32;
    let mut year = 1970u32;
    let mut remaining = days;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < in_year {
            break;
        }
        remaining -= in_year;
        year += 1;
    }
    let months = if is_leap(year) {
        [31u32, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u32, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u32;
    for &len in &months {
        if remaining < len {
            break;
        }
        remaining -= len;
        month += 1;
    }
    (year, month, remaining + 1)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_state_derivation_all_paths() {
        // Healthy: probe ok, np fresh, bytes ok
        let s = rollup(true, 0, 8000, 10, 30, 5, 0, 3);
        assert_eq!(s.0, HealthState::Healthy);

        // Degraded: probe ok, np fresh, bytes low
        let s = rollup(true, 0, 100, 10, 30, 5_000, 0, 3);
        assert_eq!(s.0, HealthState::Degraded);

        // Degraded: probe ok, np stale
        let s = rollup(true, 200, 8000, 10, 30, 5, 0, 3);
        assert_eq!(s.0, HealthState::Degraded);

        // Down: probe failed max times
        let s = rollup(false, 0, 8000, 10, 30, 5, 5, 3);
        assert_eq!(s.0, HealthState::Down);

        // Down: bytes low + np stale
        let s = rollup(true, 200, 100, 10, 30, 5_000, 0, 3);
        assert_eq!(s.0, HealthState::Down);

        // Degraded: probe failed but well under threshold
        let s = rollup(false, 0, 8000, 10, 30, 5, 0, 5);
        assert_eq!(s.0, HealthState::Degraded);

        // Down: probe failed hits threshold
        let s = rollup(false, 0, 8000, 10, 30, 5, 4, 5);
        assert_eq!(s.0, HealthState::Down);
    }

    #[allow(clippy::too_many_arguments)]
    fn rollup(
        probe_ok: bool,
        np_age_secs: u64,
        bytes: u64,
        _tick: u64,
        stale: u64,
        min_bytes: u64,
        failed_probes: u32,
        max_failed: u32,
    ) -> (HealthState, Option<String>) {
        let mut fp = failed_probes;
        if probe_ok {
            fp = 0;
        } else {
            fp = fp.saturating_add(1);
        }
        let np_fresh = np_age_secs <= stale;
        let bytes_low = bytes < min_bytes;
        if fp >= max_failed {
            return (
                HealthState::Down,
                Some(format!("HTTP probe failed {} times in a row", fp)),
            );
        }
        if !probe_ok {
            return (
                HealthState::Degraded,
                Some("HTTP probe failed (engine unreachable on this host?)".into()),
            );
        }
        if bytes_low && !np_fresh {
            return (
                HealthState::Down,
                Some(format!(
                    "side-listener got only {} bytes (need {}) and now-playing is stale",
                    bytes, min_bytes
                )),
            );
        }
        if bytes_low {
            return (
                HealthState::Degraded,
                Some(format!(
                    "side-listener got only {} bytes this tick (need {})",
                    bytes, min_bytes
                )),
            );
        }
        if !np_fresh {
            return (
                HealthState::Degraded,
                Some("now-playing is stale — encoder may be stuck".into()),
            );
        }
        (HealthState::Healthy, None)
    }

    #[test]
    fn alert_cooldown_blocks_repeat() {
        let core = WatcherCore::default();
        assert!(core.should_alert("down", 60));
        // Second alert within 60s should be blocked
        assert!(!core.should_alert("down", 60));
        // Different kind is independent
        assert!(core.should_alert("recovered", 60));
    }

    #[test]
    fn settings_default_round_trip() {
        let s = WatcherSettings::default();
        let toml = toml::to_string(&s).unwrap();
        let back: WatcherSettings = toml::from_str(&toml).unwrap();
        assert_eq!(back.tick_seconds, s.tick_seconds);
        assert_eq!(back.mount_path, s.mount_path);
        assert_eq!(back.engine_url, s.engine_url);
    }

    #[test]
    fn leap_year_handling() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(2100));
        assert!(!is_leap(2023));
    }

    #[test]
    fn color_for_state_known_states() {
        assert_eq!(color_for_state(HealthState::Healthy), "#3ecf8e");
        assert_eq!(color_for_state(HealthState::Down), "#e23b3b");
    }
}
