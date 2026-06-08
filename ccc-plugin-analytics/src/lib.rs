//! Analytics plugin: middleware that records every non-admin request into
//! a bounded in-memory ring buffer, plus an admin dashboard with rolling
//! counts, top paths, and recent errors.
//!
//! Restart-safe storage is intentionally a follow-up: the buffer
//! is process-local. This keeps the v1 footprint trivial (no DB
//! writes per request, no schema, no cleanup) while still surfacing
//! "what's happening right now."

use askama::Template;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const BUFFER_SIZE: usize = 5_000;

#[derive(Debug, Clone)]
struct RequestEvent {
    timestamp: u64, // unix seconds
    method: String,
    path: String,
    status: u16,
    duration_ms: u64,
}

#[derive(Default)]
pub struct EventBuffer {
    events: RwLock<VecDeque<RequestEvent>>,
}

impl EventBuffer {
    fn push(&self, e: RequestEvent) {
        let mut g = self.events.write().expect("poisoned");
        if g.len() >= BUFFER_SIZE {
            g.pop_front();
        }
        g.push_back(e);
    }

    fn snapshot(&self) -> Vec<RequestEvent> {
        self.events
            .read()
            .expect("poisoned")
            .iter()
            .cloned()
            .collect()
    }
    pub fn len(&self) -> usize {
        self.events.read().map(|g| g.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginAnalyticsPlugin {
    pub buffer: Arc<EventBuffer>,
}

impl Default for CccPluginAnalyticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CccPluginAnalyticsPlugin {
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(EventBuffer::default()),
        }
    }
}

impl Plugin for CccPluginAnalyticsPlugin {
    fn name(&self) -> &'static str {
        "Analytics"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn id(&self) -> String {
        "analytics".into()
    }
    fn kind(&self) -> &'static str {
        "METRICS"
    }
    fn icon(&self) -> &'static str {
        "icon-chart"
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let len = self.buffer.len();
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: None,
            detail: serde_json::json!({
                "configured": true,
                "buffer_size": BUFFER_SIZE,
                "events_in_ring": len,
                "today": { "pageviews": len, "unique_visitors": 0u64, "top_referrer": "—" },
                "this_week": { "pageviews": len, "listeners_total": 0u64 },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!(
            "Initializing Analytics: in-memory ring buffer (capacity {})",
            BUFFER_SIZE
        );
        Ok(())
    }

    // No public routes; recording is done by a middleware layer the host
    // applies to the merged app router (see `apply_recording_layer`). The
    // alternative — putting `.layer()` on this plugin's empty router —
    // would only wrap routes registered on this plugin, which is none.

    fn routes(&self, _ctx: &AppContext) -> Option<Router> {
        None
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/analytics", get(admin_dashboard))
                .with_state(AdminState {
                    buffer: self.buffer.clone(),
                }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Analytics",
            href: "/admin/analytics".into(),
            icon: "📊",
            sort: 700,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Wrap any router with the analytics recording middleware.
/// The host calls this on the assembled app before serving.
pub fn apply_recording_layer(app: Router, plugin: &CccPluginAnalyticsPlugin) -> Router {
    app.layer(axum::middleware::from_fn_with_state(
        plugin.buffer.clone(),
        record_request,
    ))
}

// ============================================================================
// Middleware
// ============================================================================

async fn record_request(
    State(buffer): State<Arc<EventBuffer>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    // Skip noise: admin static assets and analytics endpoint itself.
    let skip = path.starts_with("/admin/static/") || path == "/admin/analytics";
    let method = req.method().clone();

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    if !skip {
        buffer.push(RequestEvent {
            timestamp: unix_now(),
            method: method.to_string(),
            path,
            status: response.status().as_u16(),
            duration_ms: elapsed_ms,
        });
    }
    response
}

// ============================================================================
// Admin dashboard
// ============================================================================

#[derive(Clone)]
struct AdminState {
    buffer: Arc<EventBuffer>,
}

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
struct DashboardPage {
    buffer_size: usize,
    last_hour: usize,
    last_day: usize,
    error_count: usize,
    avg_ms: u64,
    top_paths: Vec<TopPathRow>,
    errors: Vec<ErrorRow>,
}

struct TopPathRow {
    path: String,
    count: usize,
}

struct ErrorRow {
    when_human: String,
    status: u16,
    method: String,
    path: String,
}

async fn admin_dashboard(State(state): State<AdminState>) -> Response {
    let events = state.buffer.snapshot();
    let now = unix_now();
    let cutoff_hour = now.saturating_sub(3600);
    let cutoff_day = now.saturating_sub(86_400);

    let last_hour = events.iter().filter(|e| e.timestamp >= cutoff_hour).count();
    let last_day = events.iter().filter(|e| e.timestamp >= cutoff_day).count();

    let error_count = events.iter().filter(|e| e.status >= 400).count();

    let avg_ms = if events.is_empty() {
        0
    } else {
        let total: u64 = events.iter().map(|e| e.duration_ms).sum();
        total / (events.len() as u64)
    };

    let mut path_counts: HashMap<String, usize> = HashMap::new();
    for e in events.iter().filter(|e| e.timestamp >= cutoff_hour) {
        *path_counts.entry(e.path.clone()).or_insert(0) += 1;
    }
    let mut top_paths: Vec<TopPathRow> = path_counts
        .into_iter()
        .map(|(path, count)| TopPathRow { path, count })
        .collect();
    top_paths.sort_by(|a, b| b.count.cmp(&a.count));
    top_paths.truncate(10);

    let mut errors: Vec<ErrorRow> = events
        .iter()
        .rev()
        .filter(|e| e.status >= 400)
        .take(20)
        .map(|e| ErrorRow {
            when_human: format_unix(e.timestamp),
            status: e.status,
            method: e.method.clone(),
            path: e.path.clone(),
        })
        .collect();
    errors.truncate(20);

    render(DashboardPage {
        buffer_size: BUFFER_SIZE,
        last_hour,
        last_day,
        error_count,
        avg_ms,
        top_paths,
        errors,
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
