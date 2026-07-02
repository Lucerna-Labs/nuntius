//! Ads plugin.
//!
//! Owns the Ad data model + admin CRUD UI + a public "give me one ad
//! to serve right now" JSON endpoint that other plugins call. The
//! actual splicing into podcast / video streams happens in those
//! plugins' upload pipelines (follow-up).

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus};
use ccc_database::{Ad, CccDB};
use ccc_plugin_utils::{format_unix, render, unix_now};
use rand::seq::SliceRandom;
use serde::Deserialize;
use std::any::Any;
use std::sync::Arc;

pub struct CccPluginAdsPlugin;

impl Default for CccPluginAdsPlugin {
    fn default() -> Self {
        Self
    }
}

impl Plugin for CccPluginAdsPlugin {
    fn name(&self) -> &'static str {
        "Ads"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn id(&self) -> String {
        "ads".into()
    }
    fn kind(&self) -> &'static str {
        "MONETIZE"
    }
    fn icon(&self) -> &'static str {
        "icon-megaphone"
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let placements = ctx.db.all_ads().map(|v| v.len()).unwrap_or(0);
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: None,
            detail: serde_json::json!({
                "configured": true,
                "placements": placements,
                "fill_rate": "—",
                "revenue_today": "$0.00",
                "revenue_month": "$0.00",
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!("Initializing Ads (creative store + /api/ads/next).");
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/api/ads/next", get(public_next))
                .with_state(AppState { db: ctx.db.clone() }),
        )
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/ads", get(admin_list))
                .route("/ads/new", get(admin_new_form).post(admin_create))
                .route("/ads/:id/edit", get(admin_edit_form).post(admin_update))
                .route("/ads/:id/delete", post(admin_delete))
                .with_state(AppState { db: ctx.db.clone() }),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Ads",
            href: "/admin/ads".into(),
            icon: "📣",
            sort: 900,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone)]
struct AppState {
    db: Arc<CccDB>,
}

// ============================================================================
// Public
// ============================================================================

async fn public_next(State(state): State<AppState>) -> Response {
    let ads = match state.db.all_ads() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let active: Vec<&Ad> = ads.iter().filter(|a| a.active && a.weight > 0).collect();
    if active.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }
    // Weighted pick: build a flat slice of references where each ad
    // appears `weight` times, then choose one at random.
    let mut rng = rand::thread_rng();
    let mut pool: Vec<&Ad> = Vec::new();
    for a in &active {
        for _ in 0..a.weight {
            pool.push(*a);
        }
    }
    let pick = match pool.choose(&mut rng) {
        Some(a) => *a,
        None => return StatusCode::NO_CONTENT.into_response(),
    };
    Json(serde_json::json!({
        "id": pick.id,
        "name": pick.name,
        "media_url": pick.media_url,
        "click_url": pick.click_url,
    }))
    .into_response()
}

// ============================================================================
// Admin
// ============================================================================

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage {
    ads: Vec<AdRow>,
}

struct AdRow {
    id: u64,
    name: String,
    active: bool,
    weight: u32,
    updated_human: String,
}

async fn admin_list(State(state): State<AppState>) -> Response {
    let raw = match state.db.all_ads() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Vec<AdRow> = raw
        .into_iter()
        .map(|a| AdRow {
            id: a.id,
            name: a.name,
            active: a.active,
            weight: a.weight,
            updated_human: format_unix(a.updated_at),
        })
        .collect();
    render(AdminListPage { ads: rows })
}

#[derive(Template)]
#[template(path = "admin_edit.html")]
struct AdminEditPage<'a> {
    heading: &'a str,
    form_action: String,
    name: String,
    media_url: String,
    click_url: String,
    weight: u32,
    active: bool,
    error: Option<&'a str>,
}

async fn admin_new_form() -> Response {
    render(AdminEditPage {
        heading: "New ad",
        form_action: "/admin/ads/new".into(),
        name: String::new(),
        media_url: String::new(),
        click_url: String::new(),
        weight: 1,
        active: true,
        error: None,
    })
}

async fn admin_edit_form(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let ad = match state.db.get_ad(id) {
        Ok(Some(a)) => a,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(AdminEditPage {
        heading: "Edit ad",
        form_action: format!("/admin/ads/{}/edit", id),
        name: ad.name,
        media_url: ad.media_url,
        click_url: ad.click_url.unwrap_or_default(),
        weight: ad.weight,
        active: ad.active,
        error: None,
    })
}

#[derive(Deserialize)]
struct AdForm {
    name: String,
    media_url: String,
    #[serde(default)]
    click_url: String,
    weight: u32,
    #[serde(default)]
    active: Option<String>,
}

async fn admin_create(State(state): State<AppState>, Form(form): Form<AdForm>) -> Response {
    let now = unix_now();
    let ad = Ad {
        id: now,
        name: form.name.trim().to_string(),
        media_url: form.media_url.trim().to_string(),
        click_url: optional(&form.click_url),
        active: form.active.is_some(),
        weight: form.weight.max(1),
        created_at: now,
        updated_at: now,
    };
    let _ = state.db.upsert_ad(&ad);
    Redirect::to("/admin/ads").into_response()
}

async fn admin_update(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<AdForm>,
) -> Response {
    let mut ad = match state.db.get_ad(id) {
        Ok(Some(a)) => a,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    ad.name = form.name.trim().to_string();
    ad.media_url = form.media_url.trim().to_string();
    ad.click_url = optional(&form.click_url);
    ad.weight = form.weight.max(1);
    ad.active = form.active.is_some();
    ad.updated_at = unix_now();
    let _ = state.db.upsert_ad(&ad);
    Redirect::to("/admin/ads").into_response()
}

async fn admin_delete(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_ad(id);
    Redirect::to("/admin/ads").into_response()
}

// ============================================================================
// Helpers
// ============================================================================
// render / unix_now / format_unix live in ccc-plugin-utils.

fn optional(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}
