//! Pure-Rust AI Team (Agents) plugin + persistent left-side Command Dock API
//! for Nuntius Creator Command Center.

pub mod provider;

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_database::{
    AgentBot, AgentMessage, BlogPost, CccDB, PostStatus, ProposalStatus,
};
use ccc_plugin_utils::{format_unix, render, unix_now};
use provider::{AssistantProvider, AssistantRequest, SuiteAssistantProvider};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

pub struct AgentsPlugin {
    site_name: String,
    provider: Arc<dyn AssistantProvider>,
}

impl Default for AgentsPlugin {
    fn default() -> Self {
        Self {
            site_name: "Nuntius".into(),
            provider: Arc::new(SuiteAssistantProvider),
        }
    }
}

impl AgentsPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Self {
        Self {
            site_name: cfg.site.name.clone(),
            provider: Arc::new(SuiteAssistantProvider),
        }
    }

    pub fn with_provider(mut self, provider: Arc<dyn AssistantProvider>) -> Self {
        self.provider = provider;
        self
    }
}

impl Plugin for AgentsPlugin {
    fn name(&self) -> &'static str {
        "Agents"
    }
    fn version(&self) -> &'static str {
        "0.1.0"
    }
    fn id(&self) -> String {
        "agents".into()
    }
    fn kind(&self) -> &'static str {
        "AI TEAM"
    }
    fn icon(&self) -> &'static str {
        "icon-blog"
    }
    fn admin_url(&self) -> String {
        "/admin/agents".into()
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let bots = ctx.db.all_agent_bots().unwrap_or_default();
        let pending = ctx.db.pending_agent_proposals_count().unwrap_or(0);
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: if pending > 0 {
                Some(pending.to_string())
            } else {
                None
            },
            detail: serde_json::json!({
                "configured": true,
                "bots": bots.len(),
                "pending_approvals": pending,
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!("Initializing Agents Plugin (persistent left-side AI Command Dock + approval queue).");
        Ok(())
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        let state = AgentsState {
            db: ctx.db.clone(),
            site_name: Arc::new(self.site_name.clone()),
            provider: self.provider.clone(),
        };
        Some(
            Router::new()
                .route("/agents", get(admin_bots_page))
                .route("/agents/new", get(admin_bot_new_form))
                .route("/agents/bots", post(admin_bot_create))
                .route(
                    "/agents/:id/edit",
                    get(admin_bot_edit_form).post(admin_bot_update),
                )
                .route("/agents/:id/delete", post(admin_bot_delete))
                // JSON API powering the persistent left-side AI Command Dock on every tab
                .route("/agents/api/state", get(api_dock_state))
                .route("/agents/api/send", post(api_dock_send))
                .route("/agents/api/clear", post(api_dock_clear))
                .route(
                    "/agents/api/proposals/:id/approve",
                    post(api_proposal_approve),
                )
                .route(
                    "/agents/api/proposals/:id/reject",
                    post(api_proposal_reject),
                )
                .with_state(state),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Agents",
            href: "/admin/agents".into(),
            icon: "🤖",
            sort: 240,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone)]
struct AgentsState {
    db: Arc<CccDB>,
    site_name: Arc<String>,
    provider: Arc<dyn AssistantProvider>,
}

fn ensure_starter_bots(db: &CccDB) -> Vec<AgentBot> {
    let existing = db.all_agent_bots().unwrap_or_default();
    if !existing.is_empty() {
        return existing;
    }
    let now = unix_now();
    let starters = vec![
        AgentBot {
            id: db.next_id("agent_bot").unwrap_or(1),
            name: "Editorial Director".into(),
            role: "Long-form writer, editor & multi-blog strategist".into(),
            system_prompt: "You are the Editorial Director for a solo media company running on Nuntius.".into(),
            provider: "api".into(),
            model: "default-editorial".into(),
            api_key_env: "NUNTIUS_LLM_API_KEY".into(),
            default_blog_site_id: 0,
            created_at: now,
            updated_at: now,
        },
        AgentBot {
            id: db.next_id("agent_bot").unwrap_or(2),
            name: "Suite Operations".into(),
            role: "Blogs, Mail, WordCraft & PdfCraft coordinator".into(),
            system_prompt: "You help manage all Nuntius tools, blogs, and workflows.".into(),
            provider: "api".into(),
            model: "default-ops".into(),
            api_key_env: "NUNTIUS_LLM_API_KEY".into(),
            default_blog_site_id: 0,
            created_at: now,
            updated_at: now,
        },
    ];
    for bot in &starters {
        let _ = db.upsert_agent_bot(bot);
    }
    starters
}

// ============================================================================
// HTML management pages (/admin/agents)
// ============================================================================

#[derive(Template)]
#[template(path = "bots.html")]
struct BotsPage {
    bots: Vec<AgentBot>,
}

async fn admin_bots_page(State(state): State<AgentsState>) -> Response {
    let bots = ensure_starter_bots(&state.db);
    render(BotsPage { bots })
}

struct SiteOption {
    id: u64,
    label: String,
}

fn build_site_options(state: &AgentsState) -> Vec<SiteOption> {
    let mut opts = vec![SiteOption {
        id: 0,
        label: format!("{} (Default /blog/)", state.site_name),
    }];
    if let Ok(sites) = state.db.all_blog_sites() {
        for s in sites {
            opts.push(SiteOption {
                id: s.id,
                label: s.name,
            });
        }
    }
    opts
}

#[derive(Template)]
#[template(path = "bot_edit.html")]
struct BotEditPage<'a> {
    heading: &'a str,
    form_action: String,
    delete_action: Option<String>,
    name: String,
    role: String,
    provider: String,
    model: String,
    api_key_env: String,
    default_blog_site_id: u64,
    site_options: Vec<SiteOption>,
    system_prompt: String,
}

async fn admin_bot_new_form(State(state): State<AgentsState>) -> Response {
    render(BotEditPage {
        heading: "New Bot",
        form_action: "/admin/agents/bots".into(),
        delete_action: None,
        name: String::new(),
        role: String::new(),
        provider: "api".into(),
        model: String::new(),
        api_key_env: "NUNTIUS_LLM_API_KEY".into(),
        default_blog_site_id: 0,
        site_options: build_site_options(&state),
        system_prompt: "You are an AI specialist helping run a solo media company in Nuntius.".into(),
    })
}

async fn admin_bot_edit_form(
    State(state): State<AgentsState>,
    Path(id): Path<u64>,
) -> Response {
    let bot = match state.db.get_agent_bot(id) {
        Ok(Some(b)) => b,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(BotEditPage {
        heading: "Configure Bot",
        form_action: format!("/admin/agents/{id}/edit"),
        delete_action: Some(format!("/admin/agents/{id}/delete")),
        name: bot.name,
        role: bot.role,
        provider: bot.provider,
        model: bot.model,
        api_key_env: bot.api_key_env,
        default_blog_site_id: bot.default_blog_site_id,
        site_options: build_site_options(&state),
        system_prompt: bot.system_prompt,
    })
}

#[derive(Deserialize)]
struct BotForm {
    name: String,
    #[serde(default)]
    role: String,
    provider: String,
    model: String,
    #[serde(default)]
    api_key_env: String,
    #[serde(default)]
    default_blog_site_id: u64,
    #[serde(default)]
    system_prompt: String,
}

async fn admin_bot_create(
    State(state): State<AgentsState>,
    Form(form): Form<BotForm>,
) -> Response {
    let now = unix_now();
    let id = state.db.next_id("agent_bot").unwrap_or(now);
    let bot = AgentBot {
        id,
        name: form.name.trim().to_string(),
        role: form.role.trim().to_string(),
        system_prompt: form.system_prompt.trim().to_string(),
        provider: form.provider.trim().to_string(),
        model: form.model.trim().to_string(),
        api_key_env: form.api_key_env.trim().to_string(),
        default_blog_site_id: form.default_blog_site_id,
        created_at: now,
        updated_at: now,
    };
    let _ = state.db.upsert_agent_bot(&bot);
    Redirect::to("/admin/agents").into_response()
}

async fn admin_bot_update(
    State(state): State<AgentsState>,
    Path(id): Path<u64>,
    Form(form): Form<BotForm>,
) -> Response {
    let mut prev = match state.db.get_agent_bot(id) {
        Ok(Some(b)) => b,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    prev.name = form.name.trim().to_string();
    prev.role = form.role.trim().to_string();
    prev.provider = form.provider.trim().to_string();
    prev.model = form.model.trim().to_string();
    prev.api_key_env = form.api_key_env.trim().to_string();
    prev.default_blog_site_id = form.default_blog_site_id;
    prev.system_prompt = form.system_prompt.trim().to_string();
    prev.updated_at = unix_now();
    let _ = state.db.upsert_agent_bot(&prev);
    Redirect::to("/admin/agents").into_response()
}

async fn admin_bot_delete(State(state): State<AgentsState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_agent_bot(id);
    Redirect::to("/admin/agents").into_response()
}

// ============================================================================
// Persistent Left-Side Dock JSON API (/admin/agents/api/*)
// ============================================================================

#[derive(Deserialize, Default)]
struct DockStateQuery {
    bot: Option<u64>,
}

#[derive(Serialize)]
struct DockBotDto {
    id: u64,
    name: String,
    role: String,
    provider: String,
    model: String,
}

#[derive(Serialize)]
struct DockMessageDto {
    id: u64,
    role: String,
    sender: String,
    when: String,
    html: String,
}

#[derive(Serialize)]
struct DockProposalDto {
    id: u64,
    title: String,
    slug: String,
    site_label: String,
}

#[derive(Serialize)]
struct DockStateResponse {
    bots: Vec<DockBotDto>,
    active_bot_id: u64,
    messages: Vec<DockMessageDto>,
    pending_proposals: Vec<DockProposalDto>,
}

fn build_dock_state(state: &AgentsState, requested_bot_id: Option<u64>) -> DockStateResponse {
    let bots = ensure_starter_bots(&state.db);
    let active = requested_bot_id
        .and_then(|id| bots.iter().find(|b| b.id == id).cloned())
        .or_else(|| bots.first().cloned());

    let bot_dtos: Vec<DockBotDto> = bots
        .iter()
        .map(|b| DockBotDto {
            id: b.id,
            name: b.name.clone(),
            role: b.role.clone(),
            provider: b.provider.clone(),
            model: b.model.clone(),
        })
        .collect();

    let mut site_names: HashMap<u64, String> = HashMap::new();
    site_names.insert(0, format!("{} (Default)", state.site_name));
    if let Ok(sites) = state.db.all_blog_sites() {
        for s in sites {
            site_names.insert(s.id, s.name);
        }
    }

    if let Some(bot) = active {
        let msgs = state
            .db
            .agent_messages_for_bot(bot.id)
            .unwrap_or_default()
            .into_iter()
            .map(|m| DockMessageDto {
                id: m.id,
                sender: if m.role == "user" {
                    "You".into()
                } else {
                    bot.name.clone()
                },
                role: m.role,
                when: format_unix(m.created_at),
                html: render_markdown_safe(&m.content),
            })
            .collect();

        let props = state
            .db
            .agent_proposals_for_bot(bot.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.status == ProposalStatus::Pending)
            .map(|p| DockProposalDto {
                id: p.id,
                title: p.title,
                slug: p.slug,
                site_label: site_names
                    .get(&p.site_id)
                    .cloned()
                    .unwrap_or_else(|| format!("Site #{}", p.site_id)),
            })
            .collect();

        DockStateResponse {
            bots: bot_dtos,
            active_bot_id: bot.id,
            messages: msgs,
            pending_proposals: props,
        }
    } else {
        DockStateResponse {
            bots: bot_dtos,
            active_bot_id: 0,
            messages: Vec::new(),
            pending_proposals: Vec::new(),
        }
    }
}

async fn api_dock_state(
    State(state): State<AgentsState>,
    Query(q): Query<DockStateQuery>,
) -> Response {
    Json(build_dock_state(&state, q.bot)).into_response()
}

#[derive(Deserialize)]
struct DockSendRequest {
    bot_id: u64,
    message: String,
    #[serde(default)]
    current_path: String,
}

async fn api_dock_send(
    State(state): State<AgentsState>,
    Json(req): Json<DockSendRequest>,
) -> Response {
    let bot = match state.db.get_agent_bot(req.bot_id) {
        Ok(Some(b)) => b,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let text = req.message.trim();
    if text.is_empty() {
        return Json(build_dock_state(&state, Some(bot.id))).into_response();
    }

    let now = unix_now();
    let user_msg_id = state.db.next_id("agent_msg").unwrap_or(now);
    let _ = state.db.append_agent_message(&AgentMessage {
        id: user_msg_id,
        bot_id: bot.id,
        role: "user".into(),
        content: text.to_string(),
        created_at: now,
    });

    let history = state.db.agent_messages_for_bot(bot.id).unwrap_or_default();
    let path = if req.current_path.trim().is_empty() {
        "/admin"
    } else {
        req.current_path.trim()
    };

    match state
        .provider
        .respond(AssistantRequest {
            db: &state.db,
            site_name: &state.site_name,
            bot: &bot,
            current_path: path,
            history: &history,
            user_message: text,
        })
        .await
    {
        Ok(reply) => {
            let reply_id = state.db.next_id("agent_msg").unwrap_or(unix_now());
            let _ = state.db.append_agent_message(&AgentMessage {
                id: reply_id,
                bot_id: bot.id,
                role: "assistant".into(),
                content: reply.content,
                created_at: unix_now(),
            });
            if let Some(mut prop) = reply.proposal {
                prop.id = state.db.next_id("agent_proposal").unwrap_or(unix_now());
                let _ = state.db.upsert_agent_proposal(&prop);
            }
            Json(build_dock_state(&state, Some(bot.id))).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e }))).into_response(),
    }
}

#[derive(Deserialize)]
struct DockClearRequest {
    bot_id: u64,
}

async fn api_dock_clear(
    State(state): State<AgentsState>,
    Json(req): Json<DockClearRequest>,
) -> Response {
    let _ = state.db.clear_agent_messages(req.bot_id);
    Json(build_dock_state(&state, Some(req.bot_id))).into_response()
}

async fn api_proposal_approve(
    State(state): State<AgentsState>,
    Path(id): Path<u64>,
) -> Response {
    let mut prop = match state.db.get_agent_proposal(id) {
        Ok(Some(p)) => p,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if prop.status != ProposalStatus::Pending {
        return Json(serde_json::json!({ "ok": true })).into_response();
    }
    let now = unix_now();
    let post_id = state.db.next_id("blog_post").unwrap_or(now);
    let post = BlogPost {
        id: post_id,
        slug: prop.slug.clone(),
        title: prop.title.clone(),
        content: prop.content.clone(),
        status: PostStatus::Draft,
        created_at: now,
        updated_at: now,
        published_at: None,
    };
    match state.db.upsert_blog_post_for_site(prop.site_id, &post) {
        Ok(()) => {
            prop.status = ProposalStatus::Approved;
            let _ = state.db.upsert_agent_proposal(&prop);
            Json(serde_json::json!({
                "ok": true,
                "edit_url": format!("/admin/blog/posts/{post_id}/edit"),
                "state": build_dock_state(&state, Some(prop.bot_id)),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

async fn api_proposal_reject(
    State(state): State<AgentsState>,
    Path(id): Path<u64>,
) -> Response {
    if let Ok(Some(mut prop)) = state.db.get_agent_proposal(id) {
        prop.status = ProposalStatus::Rejected;
        let _ = state.db.upsert_agent_proposal(&prop);
        return Json(serde_json::json!({
            "ok": true,
            "state": build_dock_state(&state, Some(prop.bot_id)),
        }))
        .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

fn render_markdown_safe(input: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, opts);
    let mut buf = String::with_capacity(input.len() + 64);
    html::push_html(&mut buf, parser);
    ammonia::clean(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_sanitizes_scripts() {
        let html = render_markdown_safe("Hi **team** <script>alert(1)</script>");
        assert!(html.contains("<strong>team</strong>"));
        assert!(!html.contains("<script"));
    }
}
