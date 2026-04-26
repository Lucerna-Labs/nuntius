//! Whisper transcription plugin.
//!
//! Provides:
//! - Job queue stored in redb (`TranscriptionJob`).
//! - Background worker that polls Queued jobs and shells out to a whisper
//!   CLI (default: `whisper`, OpenAI Python; whisper.cpp also works).
//! - Admin UI at `/admin/transcripts` for queueing, monitoring, and
//!   manually editing transcripts.
//!
//! Graceful degradation: if no whisper binary is on PATH, jobs stay in
//! Queued state and the admin list shows a banner explaining the
//! situation. Operator can still paste transcripts manually.

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError};
use ccc_database::{CccDB, TranscriptionJob, TranscriptionStatus};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ============================================================================
// Settings (whisper.toml sidecar)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperSettings {
    pub binary: String,
    pub default_model: String,
    pub default_language: String,
    pub output_dir: String,
}

impl Default for WhisperSettings {
    fn default() -> Self {
        Self {
            binary: "whisper".into(),
            default_model: "base".into(),
            default_language: "auto".into(),
            output_dir: "./transcripts".into(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SettingsFile {
    whisper: WhisperSettings,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<WhisperSettings>,
}

impl SettingsStore {
    pub fn load_or_init(path: &std::path::Path) -> Result<Self, PluginError> {
        let whisper = if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(io_err)?;
            let parsed: SettingsFile =
                toml::from_str(&raw).map_err(|e| PluginError::InitError(e.to_string()))?;
            parsed.whisper
        } else {
            WhisperSettings::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(whisper),
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn snapshot(&self) -> WhisperSettings {
        self.inner.read().expect("poisoned").clone()
    }

    pub fn replace(&self, new: WhisperSettings) -> Result<(), PluginError> {
        *self.inner.write().expect("poisoned") = new;
        self.persist()
    }

    fn persist(&self) -> Result<(), PluginError> {
        let snap = self.snapshot();
        let file = SettingsFile { whisper: snap };
        let raw =
            toml::to_string_pretty(&file).map_err(|e| PluginError::InitError(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io_err)?;
            }
        }
        std::fs::write(&self.path, raw).map_err(io_err)?;
        Ok(())
    }
}

fn io_err(e: std::io::Error) -> PluginError {
    PluginError::InitError(format!("whisper IO: {}", e))
}

/// Best-effort check that the configured whisper binary is on PATH.
fn binary_available(name: &str) -> bool {
    use std::process::Command;
    Command::new(name)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ============================================================================
// Plugin
// ============================================================================

pub struct CccPluginWhisperPlugin {
    settings: Arc<SettingsStore>,
}

impl CccPluginWhisperPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Result<Self, PluginError> {
        let path = cfg
            .plugin
            .get("whisper")
            .and_then(|t| t.get("settings_path"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("whisper.toml"));
        let store = Arc::new(SettingsStore::load_or_init(&path)?);
        Ok(Self { settings: store })
    }
}

impl Plugin for CccPluginWhisperPlugin {
    fn name(&self) -> &'static str {
        "Whisper Transcription"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        let snap = self.settings.snapshot();
        let avail = binary_available(&snap.binary);
        println!(
            "Initializing Whisper: binary='{}' (available: {})",
            snap.binary, avail
        );
        Ok(())
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AdminState {
            db: ctx.db.clone(),
            settings: self.settings.clone(),
        };
        // Spawn the worker once, on first admin_routes() call.
        // (admin_routes is called exactly once per plugin during startup.)
        spawn_worker(state.clone());
        Some(
            Router::new()
                .route("/transcripts", get(admin_list).post(admin_create))
                .route(
                    "/transcripts/settings",
                    get(settings_get).post(settings_post),
                )
                .route("/transcripts/:id", get(admin_show))
                .route("/transcripts/:id/transcript", post(admin_save_transcript))
                .route("/transcripts/:id/requeue", post(admin_requeue))
                .route("/transcripts/:id/delete", post(admin_delete))
                .with_state(state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Transcripts",
            href: "/admin/transcripts".into(),
            icon: "📝",
            sort: 300,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Background worker
// ============================================================================

/// Spawn the polling worker. Idempotent in practice — the manager only
/// calls admin_routes once per boot, but we guard against repeats with a
/// process-wide flag.
fn spawn_worker(state: AdminState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SPAWNED: AtomicBool = AtomicBool::new(false);
    if SPAWNED.swap(true, Ordering::SeqCst) {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let settings = state.settings.snapshot();
            if !binary_available(&settings.binary) {
                continue; // stay quiet; admin UI surfaces this
            }
            let queued = match state.db.queued_transcription_jobs() {
                Ok(v) => v,
                Err(_) => continue,
            };
            for job in queued {
                run_one(&state, job, &settings).await;
            }
        }
    });
}

async fn run_one(state: &AdminState, mut job: TranscriptionJob, settings: &WhisperSettings) {
    job.status = TranscriptionStatus::Running;
    job.updated_at = unix_now();
    let _ = state.db.upsert_transcription_job(&job);

    if let Err(e) = std::fs::create_dir_all(&settings.output_dir) {
        finish_failed(state, job, format!("could not create output dir: {}", e));
        return;
    }

    use tokio::process::Command;
    let cmd_result = Command::new(&settings.binary)
        .arg(&job.audio_path)
        .arg("--model")
        .arg(&job.model)
        .arg("--language")
        .arg(&job.language)
        .arg("--output_format")
        .arg("txt")
        .arg("--output_dir")
        .arg(&settings.output_dir)
        .output()
        .await;

    match cmd_result {
        Ok(o) if o.status.success() => {
            let txt_path = guessed_output_path(&settings.output_dir, &job.audio_path);
            let transcript = std::fs::read_to_string(&txt_path)
                .ok()
                .or_else(|| Some(String::from_utf8_lossy(&o.stdout).into_owned()));
            job.status = TranscriptionStatus::Completed;
            job.transcript = transcript;
            job.error = None;
            job.updated_at = unix_now();
            let _ = state.db.upsert_transcription_job(&job);
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
            finish_failed(state, job, format!("whisper exit {}: {}", o.status, stderr));
        }
        Err(e) => finish_failed(state, job, format!("spawn failed: {}", e)),
    }
}

fn finish_failed(state: &AdminState, mut job: TranscriptionJob, err: String) {
    job.status = TranscriptionStatus::Failed;
    job.error = Some(err);
    job.updated_at = unix_now();
    let _ = state.db.upsert_transcription_job(&job);
}

/// Whisper writes `<input-stem>.txt` into `output_dir`. Recover that path.
fn guessed_output_path(output_dir: &str, audio_path: &str) -> PathBuf {
    let stem = std::path::Path::new(audio_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let mut p = PathBuf::from(output_dir);
    p.push(format!("{}.txt", stem));
    p
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Clone)]
struct AdminState {
    db: Arc<CccDB>,
    settings: Arc<SettingsStore>,
}

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage<'a> {
    jobs: Vec<JobRow>,
    whisper_available: bool,
    whisper_binary: String,
    default_language: String,
    default_model: String,
    status_message: Option<&'a str>,
}

struct JobRow {
    id: u64,
    audio_path: String,
    language: String,
    model: String,
    status_str: &'static str,
    updated_human: String,
}

async fn admin_list(State(state): State<AdminState>) -> Response {
    let snap = state.settings.snapshot();
    let raw = match state.db.all_transcription_jobs() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Vec<JobRow> = raw
        .into_iter()
        .map(|j| JobRow {
            id: j.id,
            audio_path: j.audio_path,
            language: j.language,
            model: j.model,
            status_str: j.status.as_str(),
            updated_human: format_unix(j.updated_at),
        })
        .collect();

    render(AdminListPage {
        jobs: rows,
        whisper_available: binary_available(&snap.binary),
        whisper_binary: snap.binary,
        default_language: snap.default_language,
        default_model: snap.default_model,
        status_message: None,
    })
}

#[derive(Deserialize)]
struct CreateForm {
    audio_path: String,
    language: String,
    model: String,
}

async fn admin_create(State(state): State<AdminState>, Form(form): Form<CreateForm>) -> Response {
    let now = unix_now();
    let snap = state.settings.snapshot();
    let job = TranscriptionJob {
        id: now,
        audio_path: form.audio_path.trim().to_string(),
        status: TranscriptionStatus::Queued,
        language: if form.language.trim().is_empty() {
            snap.default_language
        } else {
            form.language.trim().to_string()
        },
        model: if form.model.trim().is_empty() {
            snap.default_model
        } else {
            form.model.trim().to_string()
        },
        transcript: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let _ = state.db.upsert_transcription_job(&job);
    Redirect::to("/admin/transcripts").into_response()
}

#[derive(Template)]
#[template(path = "admin_show.html")]
struct AdminShowPage<'a> {
    id: u64,
    audio_path: String,
    language: String,
    model: String,
    status_str: &'static str,
    transcript: String,
    placeholder: &'static str,
    error: Option<&'a str>,
    created_human: String,
    updated_human: String,
}

async fn admin_show(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
    let job = match state.db.get_transcription_job(id) {
        Ok(Some(j)) => j,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let placeholder = match job.status {
        TranscriptionStatus::Queued => "Job hasn't run yet — waiting for the worker to pick it up.",
        TranscriptionStatus::Running => "Job is running...",
        TranscriptionStatus::Failed => "Job failed. See the error above; you can edit and re-run.",
        TranscriptionStatus::Completed => "Transcript appears here once whisper finishes.",
    };
    render(AdminShowPage {
        id: job.id,
        audio_path: job.audio_path,
        language: job.language,
        model: job.model,
        status_str: job.status.as_str(),
        transcript: job.transcript.unwrap_or_default(),
        placeholder,
        error: job.error.as_deref(),
        created_human: format_unix(job.created_at),
        updated_human: format_unix(job.updated_at),
    })
}

#[derive(Deserialize)]
struct TranscriptForm {
    transcript: String,
}

async fn admin_save_transcript(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Form(form): Form<TranscriptForm>,
) -> Response {
    let mut job = match state.db.get_transcription_job(id) {
        Ok(Some(j)) => j,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    job.transcript = if form.transcript.is_empty() {
        None
    } else {
        Some(form.transcript)
    };
    job.updated_at = unix_now();
    let _ = state.db.upsert_transcription_job(&job);
    Redirect::to(&format!("/admin/transcripts/{}", id)).into_response()
}

async fn admin_requeue(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
    let mut job = match state.db.get_transcription_job(id) {
        Ok(Some(j)) => j,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    job.status = TranscriptionStatus::Queued;
    job.error = None;
    job.updated_at = unix_now();
    let _ = state.db.upsert_transcription_job(&job);
    Redirect::to(&format!("/admin/transcripts/{}", id)).into_response()
}

async fn admin_delete(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_transcription_job(id);
    Redirect::to("/admin/transcripts").into_response()
}

// ----- Settings -----

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct AdminSettingsPage {
    binary: String,
    default_model: String,
    default_language: String,
    output_dir: String,
    available: bool,
    saved: Option<bool>,
}

async fn settings_get(State(state): State<AdminState>) -> Response {
    let snap = state.settings.snapshot();
    let avail = binary_available(&snap.binary);
    render(AdminSettingsPage {
        binary: snap.binary,
        default_model: snap.default_model,
        default_language: snap.default_language,
        output_dir: snap.output_dir,
        available: avail,
        saved: None,
    })
}

#[derive(Deserialize)]
struct SettingsForm {
    binary: String,
    default_model: String,
    default_language: String,
    output_dir: String,
}

async fn settings_post(
    State(state): State<AdminState>,
    Form(form): Form<SettingsForm>,
) -> Response {
    let new = WhisperSettings {
        binary: form.binary,
        default_model: form.default_model,
        default_language: form.default_language,
        output_dir: form.output_dir,
    };
    let _ = state.settings.replace(new);
    Redirect::to("/admin/transcripts/settings?saved=1").into_response()
}

// ============================================================================
// Helpers
// ============================================================================

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
    format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", y, mo, d, h, mi)
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
