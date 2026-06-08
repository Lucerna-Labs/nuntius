//! Snowflake-style bridge rendezvous plugin.
//!
//! This plugin is the broker/rendezvous layer, not the traffic relay itself.
//! Bridges and censored clients use these short-lived endpoints to exchange
//! WebRTC SDP/ICE. The resulting peer connection carries encrypted traffic
//! directly between those peers, so the broker stays content-blind.

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_BRIDGE_TTL_SECS: u64 = 120;
const DEFAULT_SESSION_TTL_SECS: u64 = 120;
const MAX_TTL_SECS: u64 = 600;

#[derive(Default)]
pub struct BridgePlugin {
    state: Arc<BridgeState>,
}

#[derive(Default)]
struct BridgeState {
    next_id: AtomicU64,
    bridges: RwLock<HashMap<String, BridgeAdvertisement>>,
    sessions: RwLock<HashMap<String, BridgeSession>>,
}

#[derive(Debug, Clone, Serialize)]
struct BridgeAdvertisement {
    bridge_id: String,
    transport: String,
    capacity: u32,
    endpoint_hint: Option<String>,
    advertised_at: u64,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
struct BridgeSession {
    session_id: String,
    bridge_id: String,
    offer: SdpMessage,
    answer: Option<SdpMessage>,
    client_ice: Vec<IceCandidate>,
    bridge_ice: Vec<IceCandidate>,
    created_at: u64,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdpMessage {
    pub sdp_type: String,
    pub sdp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_m_line_index: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct AdvertiseRequest {
    bridge_id: Option<String>,
    transport: Option<String>,
    capacity: Option<u32>,
    endpoint_hint: Option<String>,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdvertiseResponse {
    bridge_id: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    bridge_id: String,
    offer: SdpMessage,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct AnswerRequest {
    answer: SdpMessage,
}

#[derive(Debug, Serialize)]
struct AnswerResponse {
    ready: bool,
    answer: Option<SdpMessage>,
    bridge_ice: Vec<IceCandidate>,
}

#[derive(Debug, Deserialize)]
struct IceRequest {
    candidate: IceCandidate,
}

#[derive(Debug, Deserialize)]
struct IceQuery {
    from: Option<usize>,
}

#[derive(Debug, Serialize)]
struct IceResponse {
    candidates: Vec<IceCandidate>,
    next: usize,
}

#[derive(Debug, Serialize)]
struct PendingSession {
    session_id: String,
    offer: SdpMessage,
    client_ice: Vec<IceCandidate>,
    expires_at: u64,
}

#[derive(Debug, Serialize)]
struct PublicStatus {
    active_bridges: usize,
    active_sessions: usize,
    bridge_ttl_secs: u64,
    session_ttl_secs: u64,
    content_blind: bool,
    logs_payloads: bool,
}

impl BridgeState {
    fn issue_id(&self, prefix: &str) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{}-{}", prefix, unix_now(), n)
    }

    fn cleanup_expired(&self) {
        let now = unix_now();
        if let Ok(mut bridges) = self.bridges.write() {
            bridges.retain(|_, b| b.expires_at > now);
        }
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.retain(|_, s| s.expires_at > now);
        }
    }

    fn counts(&self) -> (usize, usize) {
        self.cleanup_expired();
        let bridges = self.bridges.read().map(|g| g.len()).unwrap_or(0);
        let sessions = self.sessions.read().map(|g| g.len()).unwrap_or(0);
        (bridges, sessions)
    }
}

impl Plugin for BridgePlugin {
    fn name(&self) -> &'static str {
        "Bridge"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn id(&self) -> String {
        "bridge".into()
    }

    fn kind(&self) -> &'static str {
        "CIRCUMVENT"
    }

    fn icon(&self) -> &'static str {
        "icon-bridge"
    }

    fn admin_url(&self) -> String {
        "/admin/bridge".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let (active_bridges, active_sessions) = self.state.counts();
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: if active_bridges > 0 {
                Some(active_bridges.to_string())
            } else {
                None
            },
            detail: serde_json::json!({
                "configured": true,
                "active_bridges": active_bridges,
                "active_sessions": active_sessions,
                "content_blind": true,
                "logs_payloads": false,
                "bridge_ttl_secs": DEFAULT_BRIDGE_TTL_SECS,
                "session_ttl_secs": DEFAULT_SESSION_TTL_SECS,
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!("Initializing Bridge: content-blind WebRTC rendezvous.");
        Ok(())
    }

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(public_router(self.state.clone()))
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/bridge", get(admin_dashboard))
                .with_state(self.state.clone()),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Bridge",
            href: "/admin/bridge".into(),
            icon: "BR",
            sort: 760,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn public_router(state: Arc<BridgeState>) -> Router {
    Router::new()
        .route("/bridge/worker", get(worker_page))
        .route("/bridge/client", get(client_page))
        .route("/bridge/lab", get(lab_page))
        .route("/api/bridge/status", get(public_status))
        .route("/api/bridge/advertise", post(advertise_bridge))
        .route("/api/bridge/bridges", get(list_bridges))
        .route(
            "/api/bridge/bridges/:bridge_id/sessions",
            get(pending_sessions),
        )
        .route("/api/bridge/sessions", post(create_session))
        .route(
            "/api/bridge/sessions/:session_id/answer",
            get(get_answer).post(post_answer),
        )
        .route(
            "/api/bridge/sessions/:session_id/client-ice",
            get(get_client_ice).post(post_client_ice),
        )
        .route(
            "/api/bridge/sessions/:session_id/bridge-ice",
            get(get_bridge_ice).post(post_bridge_ice),
        )
        .with_state(state)
}

async fn worker_page() -> Html<&'static str> {
    Html(WORKER_HTML)
}

async fn client_page() -> Html<&'static str> {
    Html(CLIENT_HTML)
}

async fn lab_page() -> Html<&'static str> {
    Html(LAB_HTML)
}

async fn public_status(State(state): State<Arc<BridgeState>>) -> Response {
    let (active_bridges, active_sessions) = state.counts();
    Json(PublicStatus {
        active_bridges,
        active_sessions,
        bridge_ttl_secs: DEFAULT_BRIDGE_TTL_SECS,
        session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
        content_blind: true,
        logs_payloads: false,
    })
    .into_response()
}

async fn advertise_bridge(
    State(state): State<Arc<BridgeState>>,
    Json(req): Json<AdvertiseRequest>,
) -> Response {
    state.cleanup_expired();
    let now = unix_now();
    let bridge_id = req.bridge_id.unwrap_or_else(|| state.issue_id("bridge"));
    let ttl = req
        .ttl_secs
        .unwrap_or(DEFAULT_BRIDGE_TTL_SECS)
        .clamp(15, MAX_TTL_SECS);
    let ad = BridgeAdvertisement {
        bridge_id: bridge_id.clone(),
        transport: req.transport.unwrap_or_else(|| "webrtc".into()),
        capacity: req.capacity.unwrap_or(1).clamp(1, 64),
        endpoint_hint: req.endpoint_hint.filter(|s| !s.trim().is_empty()),
        advertised_at: now,
        expires_at: now + ttl,
    };
    if let Ok(mut bridges) = state.bridges.write() {
        bridges.insert(bridge_id.clone(), ad.clone());
    }
    Json(AdvertiseResponse {
        bridge_id,
        expires_at: ad.expires_at,
    })
    .into_response()
}

async fn list_bridges(State(state): State<Arc<BridgeState>>) -> Response {
    state.cleanup_expired();
    let mut bridges = state
        .bridges
        .read()
        .map(|g| g.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    bridges.sort_by(|a, b| b.advertised_at.cmp(&a.advertised_at));
    Json(bridges).into_response()
}

async fn create_session(
    State(state): State<Arc<BridgeState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Response {
    state.cleanup_expired();
    let bridge_exists = state
        .bridges
        .read()
        .map(|g| g.contains_key(&req.bridge_id))
        .unwrap_or(false);
    if !bridge_exists {
        return StatusCode::NOT_FOUND.into_response();
    }
    if req.offer.sdp_type != "offer" {
        return (StatusCode::BAD_REQUEST, "expected SDP offer").into_response();
    }

    let now = unix_now();
    let ttl = req
        .ttl_secs
        .unwrap_or(DEFAULT_SESSION_TTL_SECS)
        .clamp(15, MAX_TTL_SECS);
    let session_id = state.issue_id("session");
    let session = BridgeSession {
        session_id: session_id.clone(),
        bridge_id: req.bridge_id,
        offer: req.offer,
        answer: None,
        client_ice: Vec::new(),
        bridge_ice: Vec::new(),
        created_at: now,
        expires_at: now + ttl,
    };
    if let Ok(mut sessions) = state.sessions.write() {
        sessions.insert(session_id.clone(), session.clone());
    }
    Json(CreateSessionResponse {
        session_id,
        expires_at: session.expires_at,
    })
    .into_response()
}

async fn pending_sessions(
    State(state): State<Arc<BridgeState>>,
    Path(bridge_id): Path<String>,
) -> Response {
    state.cleanup_expired();
    let sessions = state
        .sessions
        .read()
        .map(|g| {
            g.values()
                .filter(|s| s.bridge_id == bridge_id && s.answer.is_none())
                .map(|s| PendingSession {
                    session_id: s.session_id.clone(),
                    offer: s.offer.clone(),
                    client_ice: s.client_ice.clone(),
                    expires_at: s.expires_at,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(sessions).into_response()
}

async fn post_answer(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
    Json(req): Json<AnswerRequest>,
) -> Response {
    state.cleanup_expired();
    if req.answer.sdp_type != "answer" {
        return (StatusCode::BAD_REQUEST, "expected SDP answer").into_response();
    }
    let mut sessions = match state.sessions.write() {
        Ok(g) => g,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let Some(session) = sessions.get_mut(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    session.answer = Some(req.answer);
    StatusCode::NO_CONTENT.into_response()
}

async fn get_answer(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
) -> Response {
    state.cleanup_expired();
    let response = state
        .sessions
        .read()
        .ok()
        .and_then(|g| g.get(&session_id).cloned())
        .map(|s| AnswerResponse {
            ready: s.answer.is_some(),
            answer: s.answer,
            bridge_ice: s.bridge_ice,
        });
    match response {
        Some(r) => Json(r).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn post_client_ice(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
    Json(req): Json<IceRequest>,
) -> Response {
    push_ice(state, session_id, req.candidate, IceSide::Client).await
}

async fn post_bridge_ice(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
    Json(req): Json<IceRequest>,
) -> Response {
    push_ice(state, session_id, req.candidate, IceSide::Bridge).await
}

async fn get_client_ice(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
    Query(q): Query<IceQuery>,
) -> Response {
    get_ice(state, session_id, q.from.unwrap_or(0), IceSide::Client).await
}

async fn get_bridge_ice(
    State(state): State<Arc<BridgeState>>,
    Path(session_id): Path<String>,
    Query(q): Query<IceQuery>,
) -> Response {
    get_ice(state, session_id, q.from.unwrap_or(0), IceSide::Bridge).await
}

enum IceSide {
    Client,
    Bridge,
}

async fn push_ice(
    state: Arc<BridgeState>,
    session_id: String,
    candidate: IceCandidate,
    side: IceSide,
) -> Response {
    state.cleanup_expired();
    let mut sessions = match state.sessions.write() {
        Ok(g) => g,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let Some(session) = sessions.get_mut(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match side {
        IceSide::Client => session.client_ice.push(candidate),
        IceSide::Bridge => session.bridge_ice.push(candidate),
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn get_ice(
    state: Arc<BridgeState>,
    session_id: String,
    from: usize,
    side: IceSide,
) -> Response {
    state.cleanup_expired();
    let response = state
        .sessions
        .read()
        .ok()
        .and_then(|g| g.get(&session_id).cloned())
        .map(|s| {
            let all = match side {
                IceSide::Client => s.client_ice,
                IceSide::Bridge => s.bridge_ice,
            };
            let start = from.min(all.len());
            let candidates = all[start..].to_vec();
            IceResponse {
                next: start + candidates.len(),
                candidates,
            }
        });
    match response {
        Some(r) => Json(r).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
struct AdminDashboardPage {
    active_bridges: usize,
    active_sessions: usize,
    bridge_ttl_secs: u64,
    session_ttl_secs: u64,
}

async fn admin_dashboard(State(state): State<Arc<BridgeState>>) -> Response {
    let (active_bridges, active_sessions) = state.counts();
    render(AdminDashboardPage {
        active_bridges,
        active_sessions,
        bridge_ttl_secs: DEFAULT_BRIDGE_TTL_SECS,
        session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
    })
}

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const WORKER_HTML: &str = include_str!("../static/worker.html");
const CLIENT_HTML: &str = include_str!("../static/client.html");
const LAB_HTML: &str = include_str!("../static/lab.html");
