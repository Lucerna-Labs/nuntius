//! Track library — upload, list, delete.
//!
//! Uploaded files land in `<library_dir>/<id>-<safe-filename>`. We
//! probe ID3/MP4/Vorbis/FLAC tags with `lofty` to populate title /
//! artist / album / genre / duration. If tags are missing we fall back
//! to the filename for title/artist and accept duration=0 (AutoDJ
//! will skip the track).

use askama::Template;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use ccc_database::Track;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

use crate::EngineState;

pub const MAX_TRACK_BYTES: usize = 200 * 1024 * 1024; // 200 MB

#[derive(Template)]
#[template(path = "admin_tracks.html")]
struct TracksPage<'a> {
    active_tab: &'static str,
    tracks: Vec<TrackRow>,
    total_human: String,
    error: Option<&'a str>,
    max_mb: usize,
}

struct TrackRow {
    id: u64,
    title: String,
    artist: String,
    duration_human: String,
    size_human: String,
    added_human: String,
    gain_human: String,
}

pub async fn admin_track_list(State(state): State<Arc<EngineState>>) -> Response {
    render_list(&state, None)
}

pub async fn admin_track_upload(
    State(state): State<Arc<EngineState>>,
    mut multipart: Multipart,
) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return render_list(&state, Some("library not available (no DB)")),
    };
    if let Err(e) = std::fs::create_dir_all(state.library_dir.as_ref()) {
        return render_list(
            &state,
            Some(&format!("could not prepare library dir: {}", e)),
        );
    }

    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut original_name: Option<String> = None;
    let mut content_type: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("audio") {
            original_name = field.file_name().map(|s| s.to_string());
            content_type = field.content_type().map(|s| s.to_string());
            match field.bytes().await {
                Ok(b) => audio_bytes = Some(b.to_vec()),
                Err(e) => return render_list(&state, Some(&format!("upload read failed: {}", e))),
            }
        } else {
            let _ = field.bytes().await;
        }
    }

    let bytes = match audio_bytes {
        Some(b) if !b.is_empty() => b,
        _ => return render_list(&state, Some("audio file is required")),
    };
    let original = original_name.unwrap_or_else(|| "track.bin".to_string());
    let mime = content_type.unwrap_or_else(|| guess_mime_from_name(&original).to_string());

    let now = unix_now();
    let id = now;
    let safe = format!("{}-{}", id, sanitize_filename(&original));
    let mut full = state.library_dir.as_ref().clone();
    full.push(&safe);

    let mut file = match tokio::fs::File::create(&full).await {
        Ok(f) => f,
        Err(e) => return render_list(&state, Some(&format!("could not write file: {}", e))),
    };
    if let Err(e) = file.write_all(&bytes).await {
        let _ = std::fs::remove_file(&full);
        return render_list(&state, Some(&format!("disk write error: {}", e)));
    }
    let _ = file.flush().await;

    // Probe metadata via lofty.
    let probed = probe_metadata(&full, &original);

    let track = Track {
        id,
        title: probed.title,
        artist: probed.artist,
        album: probed.album,
        genre: probed.genre,
        file_path: full.to_string_lossy().to_string(),
        mime_type: mime,
        size_bytes: bytes.len() as u64,
        duration_seconds: probed.duration_seconds,
        created_at: now,
        target_gain_db: None,
    };
    if let Err(e) = db.upsert_track(&track) {
        let _ = std::fs::remove_file(&full);
        return render_list(&state, Some(&format!("could not save track: {}", e)));
    }

    // Measure EBU R128 loudness in the background and store target_gain.
    // Failure (no ffmpeg, file unreadable, etc.) is non-fatal: track plays
    // ungained.
    let db_clone = db.clone();
    let path = full.clone();
    let track_id = id;
    tokio::spawn(async move {
        if let Some(gain_db) = measure_loudness_offset(&path).await {
            if let Ok(Some(mut t)) = db_clone.get_track(track_id) {
                t.target_gain_db = Some(gain_db);
                let _ = db_clone.upsert_track(&t);
                println!(
                    "[radio-engine] measured {} -> target gain {:+.1} dB",
                    path.display(),
                    gain_db
                );
            }
        } else {
            println!(
                "[radio-engine] could not measure loudness for {}",
                path.display()
            );
        }
    });

    Redirect::to("/admin/radio/tracks").into_response()
}

/// Run ffmpeg's two-pass `loudnorm` filter with `print_format=json`,
/// parse the integrated loudness, return the dB delta needed to bring
/// the track to -23 LUFS (EBU R128). On any error returns None.
async fn measure_loudness_offset(path: &PathBuf) -> Option<f32> {
    use tokio::process::Command;
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-nostats")
        .arg("-i")
        .arg(path)
        .arg("-af")
        .arg("loudnorm=I=-23:LRA=7:TP=-2:print_format=json")
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .await
        .ok()?;
    // ffmpeg's loudnorm prints a JSON block to stderr. Find it.
    let text = String::from_utf8_lossy(&output.stderr);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let measured: f32 = json
        .get("input_i")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())?;
    // gain = target − measured.  e.g. measured = -18, target = -23 => gain = -5
    Some(-23.0 - measured)
}

pub async fn admin_track_delete(
    State(state): State<Arc<EngineState>>,
    Path(id): Path<u64>,
) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return Redirect::to("/admin/radio/tracks").into_response(),
    };
    if let Ok(Some(t)) = db.get_track(id) {
        let _ = std::fs::remove_file(&t.file_path);
    }
    let _ = db.delete_track(id);
    Redirect::to("/admin/radio/tracks").into_response()
}

// ============================================================================
// Helpers
// ============================================================================

fn render_list(state: &EngineState, error: Option<&str>) -> Response {
    let db = match state.db.clone() {
        Some(db) => db,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let raw = db.all_tracks().unwrap_or_default();
    let total: u64 = raw.iter().map(|t| t.size_bytes).sum();
    let rows: Vec<TrackRow> = raw
        .into_iter()
        .map(|t| TrackRow {
            id: t.id,
            title: t.title,
            artist: t.artist,
            duration_human: format_duration(t.duration_seconds),
            size_human: human_bytes(t.size_bytes),
            added_human: format_unix(t.created_at),
            gain_human: match t.target_gain_db {
                Some(g) => format!("{:+.1} dB", g),
                None => "—".into(),
            },
        })
        .collect();
    let page = TracksPage {
        active_tab: "library",
        tracks: rows,
        total_human: human_bytes(total),
        error,
        max_mb: MAX_TRACK_BYTES / (1024 * 1024),
    };
    match page.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

struct ProbedMetadata {
    title: String,
    artist: String,
    album: String,
    genre: String,
    duration_seconds: u64,
}

fn probe_metadata(path: &PathBuf, fallback_name: &str) -> ProbedMetadata {
    use lofty::file::AudioFile;
    use lofty::file::TaggedFileExt;
    use lofty::probe::Probe;
    use lofty::tag::Accessor;

    let stem = std::path::Path::new(fallback_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let fallback = ProbedMetadata {
        title: stem.clone(),
        artist: "Unknown Artist".into(),
        album: String::new(),
        genre: String::new(),
        duration_seconds: 0,
    };

    let probe = match Probe::open(path) {
        Ok(p) => p,
        Err(_) => return fallback,
    };
    let probe = match probe.guess_file_type() {
        Ok(p) => p,
        Err(_) => return fallback,
    };
    let tagged = match probe.read() {
        Ok(t) => t,
        Err(_) => return fallback,
    };
    let duration = tagged.properties().duration().as_secs();
    let primary = tagged.primary_tag();
    let title = primary
        .and_then(|t| t.title().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or(stem);
    let artist = primary
        .and_then(|t| t.artist().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album = primary
        .and_then(|t| t.album().map(|s| s.to_string()))
        .unwrap_or_default();
    let genre = primary
        .and_then(|t| t.genre().map(|s| s.to_string()))
        .unwrap_or_default();

    ProbedMetadata {
        title,
        artist,
        album,
        genre,
        duration_seconds: duration,
    }
}

fn guess_mime_from_name(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".m4a") || lower.ends_with(".mp4") || lower.ends_with(".aac") {
        "audio/mp4"
    } else if lower.ends_with(".flac") {
        "audio/flac"
    } else if lower.ends_with(".ogg") || lower.ends_with(".oga") {
        "audio/ogg"
    } else if lower.ends_with(".opus") {
        "audio/opus"
    } else {
        "audio/mpeg"
    }
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "track.bin".into()
    } else {
        cleaned
    }
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "—".into();
    }
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
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
