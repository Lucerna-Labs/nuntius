//! Playlist CRUD + admin UI.
//!
//! AutoDJ consults `pick_active_playlist` at the top of each pass to
//! decide which playlist (if any) to airplay. Sequential playlists are
//! walked in operator order; Shuffle re-randomizes per pass.

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use ccc_database::{CccDB, Playlist, PlaylistKind, PlaylistSchedule, Track};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::EngineState;

#[derive(Template)]
#[template(path = "admin_playlists.html")]
struct ListPage<'a> {
    playlists: Vec<PlaylistRow>,
    status: Option<&'a str>,
}

struct PlaylistRow {
    id: u64,
    name: String,
    kind_str: String,
    schedule_human: String,
    priority: i32,
    track_count: usize,
}

pub async fn admin_list(State(state): State<EngineState>) -> Response {
    render_list(&state, None)
}

#[derive(Deserialize)]
pub struct CreateForm {
    name: String,
}

pub async fn admin_create(
    State(state): State<EngineState>,
    Form(form): Form<CreateForm>,
) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return Redirect::to("/admin/radio/playlists").into_response(),
    };
    let now = unix_now();
    let p = Playlist {
        id: now,
        name: form.name.trim().to_string(),
        kind: PlaylistKind::Sequential,
        schedule: PlaylistSchedule::Always,
        priority: 0,
        created_at: now,
        updated_at: now,
    };
    if let Err(e) = db.upsert_playlist(&p) {
        return render_list(&state, Some(&format!("could not save: {}", e)));
    }
    Redirect::to(&format!("/admin/radio/playlists/{}/edit", p.id)).into_response()
}

#[derive(Template)]
#[template(path = "admin_playlist_edit.html")]
struct EditPage<'a> {
    id: u64,
    name: String,
    kind_str: &'static str,
    schedule_kind: &'static str,
    start_time: String,
    end_time: String,
    day_sun: bool,
    day_mon: bool,
    day_tue: bool,
    day_wed: bool,
    day_thu: bool,
    day_fri: bool,
    day_sat: bool,
    priority: i32,
    tracks: Vec<EditTrackRow>,
    available_tracks: Vec<Track>,
    error: Option<&'a str>,
}

struct EditTrackRow {
    position: u64,
    title: String,
    artist: String,
}

pub async fn admin_edit_form(State(state): State<EngineState>, Path(id): Path<u64>) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let p = match db.get_playlist(id) {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render_edit(&db, &p, None)
}

#[derive(Deserialize)]
pub struct EditForm {
    name: String,
    kind: String,
    schedule: String,
    start_time: Option<String>,
    end_time: Option<String>,
    #[serde(default)]
    day_sun: Option<String>,
    #[serde(default)]
    day_mon: Option<String>,
    #[serde(default)]
    day_tue: Option<String>,
    #[serde(default)]
    day_wed: Option<String>,
    #[serde(default)]
    day_thu: Option<String>,
    #[serde(default)]
    day_fri: Option<String>,
    #[serde(default)]
    day_sat: Option<String>,
    priority: i32,
}

pub async fn admin_edit_post(
    State(state): State<EngineState>,
    Path(id): Path<u64>,
    Form(form): Form<EditForm>,
) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut p = match db.get_playlist(id) {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    p.name = form.name.trim().to_string();
    p.kind = PlaylistKind::from_form(&form.kind);
    p.priority = form.priority;
    p.schedule = if form.schedule == "window" {
        let start = parse_hhmm(&form.start_time.clone().unwrap_or_default()).unwrap_or(0);
        let end = parse_hhmm(&form.end_time.clone().unwrap_or_default()).unwrap_or(1440);
        let mut mask: u8 = 0;
        if form.day_sun.is_some() {
            mask |= 1;
        }
        if form.day_mon.is_some() {
            mask |= 1 << 1;
        }
        if form.day_tue.is_some() {
            mask |= 1 << 2;
        }
        if form.day_wed.is_some() {
            mask |= 1 << 3;
        }
        if form.day_thu.is_some() {
            mask |= 1 << 4;
        }
        if form.day_fri.is_some() {
            mask |= 1 << 5;
        }
        if form.day_sat.is_some() {
            mask |= 1 << 6;
        }
        if mask == 0 {
            mask = 0x7F;
        } // empty selection => every day
        PlaylistSchedule::TimeWindow {
            start_minute: start,
            end_minute: end,
            days_mask: mask,
        }
    } else {
        PlaylistSchedule::Always
    };
    p.updated_at = unix_now();
    if let Err(e) = db.upsert_playlist(&p) {
        return render_edit(&db, &p, Some(&format!("save failed: {}", e)));
    }
    Redirect::to(&format!("/admin/radio/playlists/{}/edit", id)).into_response()
}

pub async fn admin_delete(State(state): State<EngineState>, Path(id): Path<u64>) -> Response {
    if let Some(db) = state.db.clone() {
        let _ = db.delete_playlist(id);
    }
    Redirect::to("/admin/radio/playlists").into_response()
}

#[derive(Deserialize)]
pub struct AddTrackForm {
    track_id: u64,
}

pub async fn admin_add_track(
    State(state): State<EngineState>,
    Path(id): Path<u64>,
    Form(form): Form<AddTrackForm>,
) -> Response {
    if let Some(db) = state.db.clone() {
        let _ = db.append_to_playlist(id, form.track_id);
    }
    Redirect::to(&format!("/admin/radio/playlists/{}/edit", id)).into_response()
}

pub async fn admin_remove_track(
    State(state): State<EngineState>,
    Path((id, position)): Path<(u64, u64)>,
) -> Response {
    if let Some(db) = state.db.clone() {
        let _ = db.remove_from_playlist(id, position);
    }
    Redirect::to(&format!("/admin/radio/playlists/{}/edit", id)).into_response()
}

// ============================================================================
// AutoDJ playlist selection (called from autodj_loop in lib.rs)
// ============================================================================

/// Pick the highest-priority playlist whose schedule matches `now`.
/// Returns None if no playlists exist or none match — caller should
/// fall back to "all tracks shuffled" behavior.
pub fn pick_active_playlist(db: &Arc<CccDB>) -> Option<Playlist> {
    let all = db.all_playlists().ok()?;
    if all.is_empty() {
        return None;
    }
    let now = unix_now();
    let (weekday, minute_of_day) = local_weekday_and_minute(now);
    let mut matching: Vec<Playlist> = all
        .into_iter()
        .filter(|p| schedule_matches(&p.schedule, weekday, minute_of_day))
        .collect();
    if matching.is_empty() {
        return None;
    }
    matching.sort_by_key(|p| -p.priority);
    matching.into_iter().next()
}

fn schedule_matches(schedule: &PlaylistSchedule, weekday: u8, minute_of_day: u16) -> bool {
    match schedule {
        PlaylistSchedule::Always => true,
        PlaylistSchedule::TimeWindow {
            start_minute,
            end_minute,
            days_mask,
        } => {
            if (days_mask & (1 << weekday)) == 0 {
                return false;
            }
            // Handle windows that cross midnight (e.g. 22:00–02:00)
            if start_minute <= end_minute {
                minute_of_day >= *start_minute && minute_of_day < *end_minute
            } else {
                minute_of_day >= *start_minute || minute_of_day < *end_minute
            }
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn render_list(state: &EngineState, status: Option<&str>) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let raw = db.all_playlists().unwrap_or_default();
    let rows: Vec<PlaylistRow> = raw
        .into_iter()
        .map(|p| {
            let track_count = db.playlist_track_ids(p.id).map(|v| v.len()).unwrap_or(0);
            PlaylistRow {
                id: p.id,
                name: p.name,
                kind_str: p.kind.as_str().to_string(),
                schedule_human: format_schedule(&p.schedule),
                priority: p.priority,
                track_count,
            }
        })
        .collect();
    let page = ListPage {
        playlists: rows,
        status,
    };
    match page.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn render_edit(db: &Arc<CccDB>, p: &Playlist, error: Option<&str>) -> Response {
    let entries = db.playlist_track_ids(p.id).unwrap_or_default();
    let mut tracks = Vec::with_capacity(entries.len());
    for (pos, tid) in entries {
        if let Ok(Some(t)) = db.get_track(tid) {
            tracks.push(EditTrackRow {
                position: pos,
                title: t.title,
                artist: t.artist,
            });
        }
    }
    let available = db.all_tracks().unwrap_or_default();

    let (schedule_kind, start_time, end_time, mask) = match &p.schedule {
        PlaylistSchedule::Always => ("always", "00:00".to_string(), "00:00".to_string(), 0x7F_u8),
        PlaylistSchedule::TimeWindow {
            start_minute,
            end_minute,
            days_mask,
        } => (
            "window",
            minutes_to_hhmm(*start_minute),
            minutes_to_hhmm(*end_minute),
            *days_mask,
        ),
    };
    let page = EditPage {
        id: p.id,
        name: p.name.clone(),
        kind_str: p.kind.as_str(),
        schedule_kind,
        start_time,
        end_time,
        day_sun: (mask & 1) != 0,
        day_mon: (mask & (1 << 1)) != 0,
        day_tue: (mask & (1 << 2)) != 0,
        day_wed: (mask & (1 << 3)) != 0,
        day_thu: (mask & (1 << 4)) != 0,
        day_fri: (mask & (1 << 5)) != 0,
        day_sat: (mask & (1 << 6)) != 0,
        priority: p.priority,
        tracks,
        available_tracks: available,
        error,
    };
    match page.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn format_schedule(s: &PlaylistSchedule) -> String {
    match s {
        PlaylistSchedule::Always => "Always".into(),
        PlaylistSchedule::TimeWindow {
            start_minute,
            end_minute,
            days_mask,
        } => {
            format!(
                "{}–{} on {}",
                minutes_to_hhmm(*start_minute),
                minutes_to_hhmm(*end_minute),
                days_human(*days_mask)
            )
        }
    }
}

fn days_human(mask: u8) -> String {
    if mask == 0x7F {
        return "every day".into();
    }
    if mask == 0 {
        return "(no days)".into();
    }
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut out = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if (mask & (1 << i)) != 0 {
            out.push(*n);
        }
    }
    out.join(" ")
}

fn minutes_to_hhmm(m: u16) -> String {
    let h = (m / 60) % 24;
    let mm = m % 60;
    format!("{:02}:{:02}", h, mm)
}

fn parse_hhmm(s: &str) -> Option<u16> {
    let mut parts = s.split(':');
    let h: u16 = parts.next()?.parse().ok()?;
    let m: u16 = parts.next()?.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns (weekday, minute_of_day) in *server local time*.
/// Weekday: 0=Sun..6=Sat. Naively uses UTC (we don't pull a timezone
/// lib for v1; operators on UTC-offset servers can shift their windows
/// to compensate).
fn local_weekday_and_minute(unix_secs: u64) -> (u8, u16) {
    let days_since_epoch = (unix_secs / 86_400) as u32;
    let weekday = ((4 + days_since_epoch) % 7) as u8; // Jan 1 1970 was Thursday (4)... wait no, that index is for the formatter; here 0 should = Sunday.
                                                      // Recompute: 1970-01-01 was a Thursday.
                                                      // Sunday=0..Saturday=6 means Thursday=4. So weekday from days_since_epoch: (4 + days) % 7.
                                                      // But our convention has Sunday=0; Thursday=4 already matches that.
    let minute_of_day = ((unix_secs / 60) % (24 * 60)) as u16;
    (weekday, minute_of_day)
}
