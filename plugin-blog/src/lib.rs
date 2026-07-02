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
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_database::{BlogPost, CccDB, PostStatus};
use ccc_plugin_utils::{format_unix, relative_time, render, unix_now};
use serde::Deserialize;
use std::any::Any;
use std::sync::Arc;

#[derive(Default)]
pub struct StaticBlogPlugin;

impl Plugin for StaticBlogPlugin {
    fn name(&self) -> &'static str {
        "Blog"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn id(&self) -> String {
        "blog".into()
    }
    fn kind(&self) -> &'static str {
        "PUBLISHING"
    }
    fn icon(&self) -> &'static str {
        "icon-blog"
    }
    fn admin_url(&self) -> String {
        "/admin/blog/posts".into()
    }

    fn status(&self, ctx: &AppContext) -> PluginStatus {
        let posts = ctx.db.all_blog_posts().unwrap_or_default();
        let published = posts.iter().filter(|p| p.published_at.is_some()).count();
        let drafts = posts.len() - published;
        let recent: Vec<_> = posts
            .iter()
            .rev()
            .take(8)
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "title": p.title,
                    "status": if p.published_at.is_some() { "published" } else { "draft" },
                    "when": p.published_at.map(relative_time).unwrap_or_else(|| "editing".into()),
                    "views": 0u64,
                })
            })
            .collect();
        PluginStatus {
            state: PluginLifecycle::Configured,
            badge: if drafts > 0 {
                Some(drafts.to_string())
            } else {
                None
            },
            detail: serde_json::json!({
                "configured": true,
                "recent": recent,
                "counts": { "published": published, "drafts": drafts },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
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
    /// Plain-text excerpt of the post body for the og:description /
    /// twitter description meta tags.
    meta_description: String,
}

async fn public_show(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let post = match state.db.get_blog_post_by_slug(&slug) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicPost {
        title: post.title.clone(),
        rendered_html: render_post_html(&post.content),
        published_human: format_unix(post.published_at.unwrap_or(post.updated_at)),
        meta_description: markdown_excerpt(&post.content, 160),
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
// `render` lives in ccc-plugin-utils; markdown handling stays local.

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

/// Markdown -> HTML -> sanitized HTML. Markdown deliberately passes raw
/// HTML through, and the template renders the result with the `safe`
/// filter — so everything headed for the public page goes through
/// ammonia first (strips scripts, event handlers, javascript: URLs;
/// keeps ordinary formatting, links, and tables). On top of the
/// default policy we allow `id`/`class` on `div` and `class` on `sup`,
/// which pulldown-cmark's footnote output needs — the default policy
/// strips the definition's `id` and turns footnote references into
/// dead anchors.
fn render_post_html(input: &str) -> String {
    ammonia::Builder::default()
        .add_tag_attributes("div", &["id", "class"])
        .add_tag_attributes("sup", &["class"])
        .clean(&render_markdown(input))
        .to_string()
}

/// Plain-text excerpt of a markdown document for meta-description tags:
/// walks the parser's text/code events (so headings markers, emphasis,
/// and link URLs never leak through), collapses whitespace, and
/// truncates to `max_chars` characters with an ellipsis.
fn markdown_excerpt(input: &str, max_chars: usize) -> String {
    use pulldown_cmark::{Event, Parser, TagEnd};
    let mut out = String::new();
    for ev in Parser::new(input) {
        match ev {
            // Inline text flows as-is (adjacent events already carry
            // their own spacing); blocks and line breaks get a space so
            // "# Heading\n\nBody" doesn't fuse into "HeadingBody".
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Item) => out.push(' '),
            _ => {}
        }
        // Stop parsing once we clearly have enough material.
        if out.chars().count() > max_chars {
            break;
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", truncated.trim_end())
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_strips_markdown_and_truncates() {
        let md = "# Heading\n\nSome **bold** text with a [link](https://example.com/x) and `code`.";
        assert_eq!(
            markdown_excerpt(md, 160),
            "Heading Some bold text with a link and code."
        );
        let long = "word ".repeat(100);
        let excerpt = markdown_excerpt(&long, 20);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= 21);
    }

    #[test]
    fn excerpt_of_empty_body_is_empty() {
        assert_eq!(markdown_excerpt("", 160), "");
    }

    #[test]
    fn script_tags_are_stripped_from_rendered_posts() {
        let html =
            render_post_html("Intro paragraph.\n\n<script>alert('xss')</script>\n\n**bold** end");
        assert!(!html.contains("<script"), "script tag survived: {html}");
        assert!(!html.contains("alert("), "script body survived: {html}");
        // Ordinary markdown formatting must keep working.
        assert!(html.contains("<strong>bold</strong>"), "markdown lost: {html}");
    }

    #[test]
    fn event_handlers_and_js_urls_are_stripped() {
        let html = render_post_html(
            "<img src=\"x\" onerror=\"alert(1)\">\n\n<a href=\"javascript:alert(2)\">click</a>",
        );
        assert!(!html.contains("onerror"), "onerror survived: {html}");
        assert!(!html.contains("javascript:"), "javascript: URL survived: {html}");
    }

    #[test]
    fn footnote_anchors_survive_sanitization() {
        let html = render_post_html("A claim.[^1]\n\n[^1]: The footnote text.");
        // The reference link and the definition's id must both survive,
        // otherwise clicking the footnote number goes nowhere.
        assert!(html.contains("href=\"#1\""), "footnote ref lost: {html}");
        assert!(html.contains("id=\"1\""), "footnote definition id lost: {html}");
        // The relaxed policy must not weaken script stripping.
        let attack = render_post_html("<div id=\"x\" onclick=\"alert(1)\"><script>bad()</script></div>");
        assert!(!attack.contains("onclick"), "onclick survived: {attack}");
        assert!(!attack.contains("<script"), "script survived: {attack}");
        assert!(attack.contains("id=\"x\""), "benign id should survive: {attack}");
    }
}
