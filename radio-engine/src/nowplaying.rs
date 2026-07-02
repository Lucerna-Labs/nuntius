//! WebSocket / SSE feed of now-playing state for live UI updates.
//!
//! Two endpoints, both share a single subscription to the engine's
//! typed now-playing broadcast channel:
//!
//! - `GET /api/radio/nowplaying/stream` — WebSocket. Each `NowPlayingEvent`
//!   becomes a text frame whose body is the JSON event.
//! - `GET /api/radio/nowplaying/sse` — Server-Sent Events. Same payload,
//!   each event becomes a `data: <json>\n\n` line.
//!
//! WebSocket is the default for browser clients (Safari, Chrome, Firefox
//! all ship first-class support). SSE is the fallback for clients that
//! can't open a WS (curl, simple scripts, RSS-aggregator-style readers).
//!
//! On connect, the server immediately sends one "snapshot" event with
//! the current now-playing state so clients render correctly without
//! waiting for the next track change.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use ccc_database::Track;
use futures::StreamExt;
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

use crate::{EngineState, NowPlaying, RecordingHandle, UpNext};

/// What gets pushed on the wire. One of two variants:
/// - `Snapshot` on connect (so new clients render immediately)
/// - `Update` on every now-playing change
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NowPlayingEvent {
    Snapshot {
        now_playing: Option<NowPlaying>,
        history: Vec<NowPlaying>,
        live: bool,
        relay: bool,
        autodj_paused: bool,
        recording: Option<String>,
        listeners: usize,
        stream_name: String,
        mount_path: String,
        radio_port: u16,
        up_next: Option<UpNext>,
        active_playlist: Option<String>,
        ts: u64,
    },
    Update {
        now_playing: Option<NowPlaying>,
        live: bool,
        relay: bool,
        autodj_paused: bool,
        listeners: usize,
        up_next: Option<UpNext>,
        active_playlist: Option<String>,
        ts: u64,
    },
    #[allow(dead_code)]
    TrackEnded {
        /// The track that just ended. Listeners use this to trigger
        /// a "track change" animation even before the next one starts.
        title: String,
        artist: String,
        ended_at: u64,
    },
}

impl NowPlayingEvent {
    pub(crate) fn snapshot(state: &EngineState) -> Self {
        let snap = state.now_playing.read().ok().and_then(|g| g.clone());
        let history = state
            .history
            .read()
            .ok()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default();
        let s = state.settings.snapshot();
        let recording = state
            .recording
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|h: &RecordingHandle| h.path.clone()));
        Self::Snapshot {
            now_playing: snap,
            history,
            live: state.source_alive.load(std::sync::atomic::Ordering::SeqCst),
            relay: state.relay_alive.load(std::sync::atomic::Ordering::SeqCst),
            autodj_paused: state
                .autodj_paused
                .load(std::sync::atomic::Ordering::SeqCst),
            recording,
            listeners: state
                .listener_count
                .load(std::sync::atomic::Ordering::SeqCst),
            stream_name: s.stream_name,
            mount_path: s.mount_path,
            radio_port: s.port,
            up_next: crate::compute_up_next(state),
            active_playlist: crate::active_playlist_name(state),
            ts: unix_now(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn update(state: &EngineState) -> Self {
        let snap = state.now_playing.read().ok().and_then(|g| g.clone());
        Self::Update {
            now_playing: snap,
            live: state.source_alive.load(std::sync::atomic::Ordering::SeqCst),
            relay: state.relay_alive.load(std::sync::atomic::Ordering::SeqCst),
            autodj_paused: state
                .autodj_paused
                .load(std::sync::atomic::Ordering::SeqCst),
            listeners: state
                .listener_count
                .load(std::sync::atomic::Ordering::SeqCst),
            up_next: crate::compute_up_next(state),
            active_playlist: crate::active_playlist_name(state),
            ts: unix_now(),
        }
    }
}

/// Build a `NowPlayingEvent` from a Track that just started. Caller hands
/// it to the broadcast channel via the same path the AutoDJ / live source
/// already use to set `now_playing`. If `prev_track` is provided, a
/// `TrackEnded` event is emitted first so listeners see a clean handoff.
#[allow(dead_code)]
pub fn build_update_event(state: &EngineState, new_np: Option<NowPlaying>) -> NowPlayingEvent {
    let _ = state; // state not strictly needed but kept for symmetry with snapshot()
    NowPlayingEvent::Update {
        now_playing: new_np,
        live: false,
        relay: false,
        autodj_paused: false,
        listeners: 0,
        up_next: None,
        active_playlist: None,
        ts: unix_now(),
    }
}

/// Holder for the typed broadcast channel that the engine emits on.
/// Stored on the EngineState as `np_events: broadcast::Sender<NowPlayingEvent>`.
pub type NpEventSender = broadcast::Sender<NowPlayingEvent>;

/// Construct a fresh channel + receiver. Used in EngineState::new.
pub fn channel() -> (NpEventSender, broadcast::Receiver<NowPlayingEvent>) {
    let (tx, rx) = broadcast::channel(64);
    (tx, rx)
}

/// WebSocket handler. Upgrades the connection and pumps events.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<EngineState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: Arc<EngineState>) {
    // 1. Send the snapshot immediately so the client renders the
    //    current state without waiting for the next track change.
    let snap = NowPlayingEvent::snapshot(&state);
    if let Ok(json) = serde_json::to_string(&snap) {
        if socket.send(Message::Text(json)).await.is_err() {
            return;
        }
    }
    // 2. Subscribe to the broadcast channel.
    let tx: NpEventSender = {
        let guard = state.np_events.read().ok();
        match guard.and_then(|g| g.clone()) {
            Some(tx) => tx,
            None => return,
        }
    };
    let mut rx = tx.subscribe();
    // 3. Pump events until the client disconnects.
    loop {
        tokio::select! {
            biased;
            // We don't expect any inbound messages, but if a client
            // sends a ping or close, we honor it.
            inbound = socket.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(Message::Ping(p))) => {
                        if socket.send(Message::Pong(p)).await.is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            event = rx.recv() => match event {
                Ok(ev) => {
                    let json = match serde_json::to_string(&ev) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if socket.send(Message::Text(json)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// Server-Sent Events handler. Sends one snapshot then streams events
/// as they happen. SSE clients reconnect on disconnect; the snapshot
/// gives them a fresh view each time.
pub async fn sse_handler(
    State(state): State<Arc<EngineState>>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let snap = NowPlayingEvent::snapshot(&state);
    let snap_json = serde_json::to_string(&snap).unwrap_or_default();
    let mut rx = {
        let guard = state.np_events.read().ok();
        match guard.and_then(|g| g.clone()) {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    };
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().data(snap_json));
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let json = serde_json::to_string(&ev).unwrap_or_default();
                    yield Ok(Event::default().data(json));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

/// Helper: build a NowPlaying from a Track that just started playing.
#[allow(dead_code)]
pub fn np_for_track(track: &Track) -> NowPlaying {
    NowPlaying {
        source: "autodj",
        track_id: Some(track.id),
        title: track.title.clone(),
        artist: track.artist.clone(),
        started_at: unix_now(),
        duration_seconds: track.duration_seconds,
    }
}

/// Helper: emit an Update event onto the engine's broadcast channel.
/// No-op if the channel isn't installed (e.g. legacy CLI subcommand).
pub(crate) fn emit_update(state: &EngineState, np: Option<NowPlaying>) {
    let guard = state.np_events.read().ok();
    if let Some(Some(tx)) = guard.as_ref().map(|g| g.as_ref()) {
        let ev = NowPlayingEvent::update_with(state, np);
        let _ = tx.send(ev);
    }
}

impl NowPlayingEvent {
    /// Build an `Update` event with fresh state values, optionally
    /// overriding `now_playing` with the supplied Option.
    pub(crate) fn update_with(state: &EngineState, np: Option<NowPlaying>) -> Self {
        let snap = match np {
            Some(n) => Some(n),
            None => state.now_playing.read().ok().and_then(|g| g.clone()),
        };
        Self::Update {
            now_playing: snap,
            live: state.source_alive.load(std::sync::atomic::Ordering::SeqCst),
            relay: state.relay_alive.load(std::sync::atomic::Ordering::SeqCst),
            autodj_paused: state
                .autodj_paused
                .load(std::sync::atomic::Ordering::SeqCst),
            listeners: state
                .listener_count
                .load(std::sync::atomic::Ordering::SeqCst),
            up_next: crate::compute_up_next(state),
            active_playlist: crate::active_playlist_name(state),
            ts: unix_now(),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serializes_to_json() {
        let ev = NowPlayingEvent::Snapshot {
            now_playing: None,
            history: vec![],
            live: false,
            relay: false,
            autodj_paused: false,
            recording: None,
            listeners: 0,
            stream_name: "Test".into(),
            mount_path: "/listen.mp3".into(),
            radio_port: 8000,
            up_next: None,
            active_playlist: None,
            ts: 0,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"snapshot\""));
        assert!(json.contains("\"stream_name\":\"Test\""));
    }

    #[test]
    fn track_ended_event_carries_metadata() {
        let ev = NowPlayingEvent::TrackEnded {
            title: "Bohemian Rhapsody".into(),
            artist: "Queen".into(),
            ended_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&ev).unwrap();
        // serde's rename_all = "lowercase" maps TrackEnded to "trackended"
        // (no underscore). Tag name is just the lowercased variant.
        assert!(json.contains("\"type\":\"trackended\""));
        assert!(json.contains("\"title\":\"Bohemian Rhapsody\""));
        assert!(json.contains("\"ended_at\":1700000000"));
    }
}
