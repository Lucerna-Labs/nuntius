//! Blog plugin: DB-backed posts, markdown rendering, admin CRUD UI.
//!
//! Surfaces:
//! - Public  `GET /blog/`            — list of published posts
//! - Public  `GET /blog/:slug`       — single rendered post
//! - Admin   `GET /admin/blog/posts` — table of all posts (drafts + published)
//! - Admin   `GET /admin/blog/posts/new`
//! - Admin   `POST /admin/blog/posts`
//! - Admin   `GET /admin/blog/posts/:id/edit`
//! - Admin   `POST /admin/blog/posts/:id`
//! - Admin   `POST /admin/blog/posts/:id/delete`

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError};
use ccc_database::{BlogPost, CccDB, PostStatus};
use serde::Deserialize;
use std::any::Any;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct StaticBlogPlugin;

impl Plugin for StaticBlogPlugin {
    fn name(&self) -> &'static str {
        "Blog"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing Blog Plugin (DB-backed posts + markdown).");
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(public_router(ctx.db.clone(), ctx.events.clone()))
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(admin_router(ctx.db.clone(), ctx.events.clone()))
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Blog",
            href: "/admin/blog/posts".into(),
            icon: "📝",
            sort: 100,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Public-facing routes
// ============================================================================

#[derive(Clone)]
struct AppState {
    db: Arc<CccDB>,
    events: Arc<EventBus>,
}

fn public_router(db: Arc<CccDB>, events: Arc<EventBus>) -> Router {
    Router::new()
        .route("/blog/", get(public_index))
        .route("/blog/:slug", get(public_show))
        .with_state(AppState { db, events })
}

#[derive(Template)]
#[template(path = "public_index.html")]
struct PublicIndex {
    posts: Vec<PublicPostSummary>,
}

struct PublicPostSummary {
    slug: String,
    title: String,
    published_human: String,
}

async fn public_index(State(state): State<AppState>) -> Response {
    let raw = match state.db.latest_published_blog_posts(50) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let posts = raw
        .into_iter()
        .map(|p| PublicPostSummary {
            slug: p.slug,
            title: p.title,
            published_human: format_unix(p.published_at.unwrap_or(p.updated_at)),
        })
        .collect();
    render(PublicIndex { posts })
}

#[derive(Template)]
#[template(path = "public_post.html")]
struct PublicPost {
    title: String,
    rendered_html: String,
    published_human: String,
}

async fn public_show(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let post = match state.db.get_blog_post_by_slug(&slug) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicPost {
        title: post.title.clone(),
        rendered_html: render_markdown(&post.content),
        published_human: format_unix(post.published_at.unwrap_or(post.updated_at)),
    })
}

// ============================================================================
// Admin routes (mounted under /admin)
// ============================================================================

fn admin_router(db: Arc<CccDB>, events: Arc<EventBus>) -> Router {
    // Paths are *relative* to the /admin nest prefix the CMS owns.
    // The full URL becomes /admin/blog/posts etc.
    Router::new()
        .route("/blog/posts", get(admin_list).post(admin_create))
        .route("/blog/posts/new", get(admin_new_form))
        .route(
            "/blog/posts/:id/edit",
            get(admin_edit_form).post(admin_update),
        )
        .route("/blog/posts/:id/delete", post(admin_delete))
        .with_state(AppState { db, events })
}

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage {
    posts: Vec<AdminPostRow>,
}

struct AdminPostRow {
    id: u64,
    title: String,
    slug: String,
    status_str: &'static str,
    updated_human: String,
}

async fn admin_list(State(state): State<AppState>) -> Response {
    let posts = match state.db.all_blog_posts() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Vec<AdminPostRow> = posts
        .into_iter()
        .map(|p| AdminPostRow {
            id: p.id,
            title: p.title,
            slug: p.slug,
            status_str: p.status.as_str(),
            updated_human: format_unix(p.updated_at),
        })
        .collect();
    render(AdminListPage { posts: rows })
}

#[derive(Template)]
#[template(path = "admin_edit.html")]
struct AdminEditPage<'a> {
    heading: &'a str,
    form_action: String,
    title: String,
    slug: String,
    slug_or_placeholder: String,
    content: String,
    status_str: &'static str,
    error: Option<&'a str>,
}

async fn admin_new_form() -> Response {
    render(AdminEditPage {
        heading: "New post",
        form_action: "/admin/blog/posts".into(),
        title: String::new(),
        slug: String::new(),
        slug_or_placeholder: "your-slug".into(),
        content: String::new(),
        status_str: "draft",
        error: None,
    })
}

async fn admin_edit_form(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let post = match state.db.get_blog_post(id) {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(AdminEditPage {
        heading: "Edit post",
        form_action: format!("/admin/blog/posts/{}/edit", id),
        title: post.title,
        slug: post.slug.clone(),
        slug_or_placeholder: post.slug,
        content: post.content,
        status_str: post.status.as_str(),
        error: None,
    })
}

#[derive(Deserialize)]
struct PostForm {
    title: String,
    slug: String,
    content: String,
    status: String,
}

async fn admin_create(State(state): State<AppState>, Form(form): Form<PostForm>) -> Response {
    let now = unix_now();
    let id = now; // unix-second-resolution id; collisions are not a real concern for human input
    let status = PostStatus::from_form(&form.status);
    let post = BlogPost {
        id,
        slug: form.slug.trim().to_string(),
        title: form.title.trim().to_string(),
        content: form.content,
        status,
        created_at: now,
        updated_at: now,
        published_at: if status == PostStatus::Published {
            Some(now)
        } else {
            None
        },
    };
    match state.db.upsert_blog_post(&post) {
        Ok(()) => {
            if status == PostStatus::Published {
                state
                    .events
                    .publish(Event::BlogPostPublished { id: post.id });
            }
            Redirect::to("/admin/blog/posts").into_response()
        }
        Err(e) => render_create_error(post, format!("Could not save: {}", e)),
    }
}

fn render_create_error(post: BlogPost, msg: String) -> Response {
    render(AdminEditPage {
        heading: "New post",
        form_action: "/admin/blog/posts".into(),
        title: post.title.clone(),
        slug: post.slug.clone(),
        slug_or_placeholder: post.slug,
        content: post.content,
        status_str: post.status.as_str(),
        error: Some(&msg),
    })
}

async fn admin_update(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<PostForm>,
) -> Response {
    let mut prev = match state.db.get_blog_post(id) {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let now = unix_now();
    let new_status = PostStatus::from_form(&form.status);

    let was_already_published = prev.status == PostStatus::Published;
    prev.title = form.title.trim().to_string();
    prev.slug = form.slug.trim().to_string();
    prev.content = form.content;
    prev.status = new_status;
    prev.updated_at = now;
    if new_status == PostStatus::Published && !was_already_published {
        prev.published_at = Some(now);
    } else if new_status == PostStatus::Draft {
        prev.published_at = None;
    }
    let response = save_or_re_render(&state, prev.clone(), "/admin/blog/posts", "Edit post").await;
    // Emit event only on the Draft -> Published transition. (Re-saving an
    // already-published post does not re-trigger newsletters.)
    if new_status == PostStatus::Published && !was_already_published {
        state.events.publish(Event::BlogPostPublished { id });
    }
    response
}

async fn save_or_re_render(
    state: &AppState,
    post: BlogPost,
    redirect: &str,
    heading: &'static str,
) -> Response {
    match state.db.upsert_blog_post(&post) {
        Ok(()) => Redirect::to(redirect).into_response(),
        Err(e) => {
            let msg = format!("Could not save: {}", e);
            render(AdminEditPage {
                heading,
                form_action: if heading == "New post" {
                    "/admin/blog/posts".into()
                } else {
                    format!("/admin/blog/posts/{}/edit", post.id)
                },
                title: post.title.clone(),
                slug: post.slug.clone(),
                slug_or_placeholder: post.slug,
                content: post.content,
                status_str: post.status.as_str(),
                error: Some(&msg),
            })
        }
    }
}

async fn admin_delete(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_blog_post(id);
    Redirect::to("/admin/blog/posts").into_response()
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

fn render_markdown(input: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(input, opts);
    let mut buf = String::with_capacity(input.len() + input.len() / 4);
    html::push_html(&mut buf, parser);
    buf
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_unix(ts: u64) -> String {
    // Minimal human-friendly date — avoids pulling chrono just for this.
    // For a real product we'd use `time` (already a transitive dep) for
    // formatting. For now: ISO-ish UTC string.
    use std::time::{Duration, UNIX_EPOCH};
    let d = UNIX_EPOCH + Duration::from_secs(ts);
    let secs_since_epoch = d.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (year, month, day, hour, minute) = unix_to_ymdhm(secs_since_epoch);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hour, minute
    )
}

/// Tiny, dependency-free Unix-seconds → (Y, M, D, h, m) for display.
/// Good for UTC, gregorian calendar, years 1970..2099.
fn unix_to_ymdhm(secs: u64) -> (u32, u32, u32, u32, u32) {
    let minutes_total = secs / 60;
    let hours_total = minutes_total / 60;
    let days_since_epoch = (hours_total / 24) as u32;
    let minute = (minutes_total % 60) as u32;
    let hour = (hours_total % 24) as u32;

    // Walk forward year by year from 1970.
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
    let months = if is_leap(year) { months_leap } else { months_normal };
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
