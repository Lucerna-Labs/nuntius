//! Multi-domain / multi-subdomain Blog plugin: DB-backed blogs & posts,
//! markdown rendering, host-based routing, and admin CRUD UI.
//!
//! Surfaces:
//! - Public  `GET /blog/`                       — default blog (or host-matched blog) index
//! - Public  `GET /blog/:slug`                  — default blog (or host-matched blog) post
//! - Public  `GET /sites/:site_slug/`           — preview/direct index for any blog site
//! - Public  `GET /sites/:site_slug/:post_slug` — preview/direct post for any blog site
//! - Host middleware (`apply_multisite_blog_layer`):
//!   When `Host` matches a configured `BlogSite`'s domain/subdomain:
//!   - `GET /` or `GET /blog/`                  — serves that blog's standalone homepage
//!   - `GET /posts/:slug` or `GET /blog/:slug`  — serves that blog's post
//! - Admin   `GET /admin/blog/posts`            — blogs overview + posts table (optional `?site_id=`)
//! - Admin   `GET /admin/blog/sites/new`        — create blog site form
//! - Admin   `POST /admin/blog/sites`           — create blog site
//! - Admin   `GET/POST /admin/blog/sites/:id/edit`
//! - Admin   `POST /admin/blog/sites/:id/delete`
//! - Admin   `GET /admin/blog/posts/new`        — new post form (optional `?site_id=`)
//! - Admin   `POST /admin/blog/posts`           — create post
//! - Admin   `GET/POST /admin/blog/posts/:id/edit`
//! - Admin   `POST /admin/blog/posts/:id/delete`

use askama::Template;
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, Event, EventBus, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_database::{normalize_host, BlogPost, BlogSite, CccDB, PostStatus};
use ccc_plugin_utils::{format_unix, relative_time, render, unix_now};
use serde::Deserialize;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// Blog plugin. Carries the operator-configured default site name (`[site] name`
/// in `ccc.toml`) for the legacy default blog (`site_id = 0`), while also
/// supporting unlimited distinct multi-domain / multi-subdomain `BlogSite`s.
pub struct StaticBlogPlugin {
    site_name: String,
}

impl Default for StaticBlogPlugin {
    fn default() -> Self {
        Self {
            site_name: "Nuntius".into(),
        }
    }
}

impl StaticBlogPlugin {
    pub fn from_config(cfg: &ccc_core::CccConfig) -> Self {
        Self {
            site_name: cfg.site.name.clone(),
        }
    }
}

impl Plugin for StaticBlogPlugin {
    fn name(&self) -> &'static str {
        "Blog"
    }

    fn version(&self) -> &'static str {
        "0.3.0"
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
        let sites = ctx.db.all_blog_sites().unwrap_or_default();
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
                "counts": { "published": published, "drafts": drafts, "sites": sites.len() + 1 },
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!("Initializing Blog Plugin (multi-domain blogs + DB-backed posts + markdown).");
        Ok(())
    }

    fn routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(public_router(
            ctx.db.clone(),
            ctx.events.clone(),
            self.site_name.clone(),
        ))
    }

    fn admin_routes(&self, ctx: &AppContext) -> Option<Router> {
        Some(admin_router(
            ctx.db.clone(),
            ctx.events.clone(),
            self.site_name.clone(),
        ))
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Blogs",
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
// Multi-domain / multi-subdomain Host routing middleware
// ============================================================================

/// Wrap the assembled application router so requests arriving with a `Host`
/// header matching any configured `BlogSite` serve that blog's standalone
/// homepage at `/` and posts at `/posts/:slug` (or `/blog/:slug`), while
/// leaving `/admin/*`, `/assets/*`, `/health*`, and non-matching hosts
/// untouched.
pub fn apply_multisite_blog_layer(app: Router, db: Arc<CccDB>) -> Router {
    app.layer(middleware::from_fn_with_state(db, multisite_host_middleware))
}

fn extract_request_host(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(header::HOST)
        .or_else(|| headers.get("x-forwarded-host"))
        .and_then(|v| v.to_str().ok())?;
    let host = normalize_host(raw);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

async fn multisite_host_middleware(
    State(db): State<Arc<CccDB>>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return next.run(req).await;
    }

    let path = req.uri().path();
    // Never intercept admin, static assets, health checks, or API paths.
    if path.starts_with("/admin")
        || path.starts_with("/assets")
        || path.starts_with("/health")
        || path.starts_with("/api")
        || path.starts_with("/version")
        || path.starts_with("/sites/")
    {
        return next.run(req).await;
    }

    if let Some(host) = extract_request_host(req.headers()) {
        if let Ok(Some(site)) = db.get_blog_site_by_host(&host) {
            if path == "/" || path == "/blog" || path == "/blog/" {
                return render_site_index(&db, &site, "/", "/posts/");
            }
            if let Some(slug) = path
                .strip_prefix("/posts/")
                .or_else(|| path.strip_prefix("/blog/"))
            {
                let slug = slug.trim_matches('/');
                if !slug.is_empty() && !slug.contains('/') {
                    return render_site_post(&db, &site, slug, "/", "/");
                }
            }
        }
    }

    next.run(req).await
}

// ============================================================================
// Public-facing routes
// ============================================================================

#[derive(Clone)]
struct AppState {
    db: Arc<CccDB>,
    events: Arc<EventBus>,
    site_name: Arc<String>,
}

fn public_router(db: Arc<CccDB>, events: Arc<EventBus>, site_name: String) -> Router {
    Router::new()
        .route("/blog/", get(public_index))
        .route("/blog/:slug", get(public_show))
        .route("/sites/:site_slug", get(public_site_index_redirect))
        .route("/sites/:site_slug/", get(public_site_index))
        .route("/sites/:site_slug/:post_slug", get(public_site_post))
        .with_state(AppState {
            db,
            events,
            site_name: Arc::new(site_name),
        })
}

fn theme_accent_hex(theme: &str) -> &'static str {
    match theme {
        "phosphor" => "#3ddbc4",
        "ink" => "#5b9eff",
        _ => "#f5a524",
    }
}

#[derive(Template)]
#[template(path = "public_index.html")]
struct PublicIndex {
    site_name: String,
    site_description: String,
    accent_hex: &'static str,
    home_href: String,
    post_base_href: String,
    standalone_site: bool,
    posts: Vec<PublicPostSummary>,
}

struct PublicPostSummary {
    slug: String,
    title: String,
    published_human: String,
}

#[derive(Template)]
#[template(path = "public_post.html")]
struct PublicPost {
    site_name: String,
    accent_hex: &'static str,
    home_href: String,
    back_href: String,
    standalone_site: bool,
    title: String,
    rendered_html: String,
    published_human: String,
    meta_description: String,
}

fn render_site_index(
    db: &CccDB,
    site: &BlogSite,
    home_href: &str,
    post_base_href: &str,
) -> Response {
    let raw = match db.latest_published_blog_posts_for_site(site.id, 50) {
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
    let desc = if site.description.is_empty() {
        format!("Latest posts from {}.", site.name)
    } else {
        site.description.clone()
    };
    render(PublicIndex {
        site_name: site.name.clone(),
        site_description: desc,
        accent_hex: theme_accent_hex(&site.theme),
        home_href: home_href.to_string(),
        post_base_href: post_base_href.to_string(),
        standalone_site: true,
        posts,
    })
}

fn render_site_post(
    db: &CccDB,
    site: &BlogSite,
    slug: &str,
    home_href: &str,
    back_href: &str,
) -> Response {
    let post = match db.get_blog_post_by_site_and_slug(site.id, slug) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicPost {
        site_name: site.name.clone(),
        accent_hex: theme_accent_hex(&site.theme),
        home_href: home_href.to_string(),
        back_href: back_href.to_string(),
        standalone_site: true,
        title: post.title.clone(),
        rendered_html: render_post_html(&post.content),
        published_human: format_unix(post.published_at.unwrap_or(post.updated_at)),
        meta_description: markdown_excerpt(&post.content, 160),
    })
}

async fn public_index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(host) = extract_request_host(&headers) {
        if let Ok(Some(site)) = state.db.get_blog_site_by_host(&host) {
            return render_site_index(&state.db, &site, "/", "/posts/");
        }
    }
    let raw = match state.db.latest_published_blog_posts_for_site(0, 50) {
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
    render(PublicIndex {
        site_name: state.site_name.as_ref().clone(),
        site_description: format!("Latest posts from {}.", state.site_name),
        accent_hex: theme_accent_hex("signal"),
        home_href: "/".into(),
        post_base_href: "/blog/".into(),
        standalone_site: false,
        posts,
    })
}

async fn public_show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Some(host) = extract_request_host(&headers) {
        if let Ok(Some(site)) = state.db.get_blog_site_by_host(&host) {
            return render_site_post(&state.db, &site, &slug, "/", "/");
        }
    }
    let post = match state.db.get_blog_post_by_site_and_slug(0, &slug) {
        Ok(Some(p)) if p.status == PostStatus::Published => p,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(PublicPost {
        site_name: state.site_name.as_ref().clone(),
        accent_hex: theme_accent_hex("signal"),
        home_href: "/".into(),
        back_href: "/blog/".into(),
        standalone_site: false,
        title: post.title.clone(),
        rendered_html: render_post_html(&post.content),
        published_human: format_unix(post.published_at.unwrap_or(post.updated_at)),
        meta_description: markdown_excerpt(&post.content, 160),
    })
}

async fn public_site_index_redirect(Path(site_slug): Path<String>) -> Response {
    Redirect::permanent(&format!("/sites/{}/", site_slug)).into_response()
}

async fn public_site_index(
    State(state): State<AppState>,
    Path(site_slug): Path<String>,
) -> Response {
    let site = match state.db.get_blog_site_by_slug(&site_slug) {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let base = format!("/sites/{}/", site.slug);
    render_site_index(&state.db, &site, &base, &base)
}

async fn public_site_post(
    State(state): State<AppState>,
    Path((site_slug, post_slug)): Path<(String, String)>,
) -> Response {
    let site = match state.db.get_blog_site_by_slug(&site_slug) {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let base = format!("/sites/{}/", site.slug);
    render_site_post(&state.db, &site, &post_slug, &base, &base)
}

// ============================================================================
// Admin routes (mounted under /admin)
// ============================================================================

fn admin_router(db: Arc<CccDB>, events: Arc<EventBus>, site_name: String) -> Router {
    Router::new()
        .route("/blog/posts", get(admin_list).post(admin_create))
        .route("/blog/posts/new", get(admin_new_form))
        .route(
            "/blog/posts/:id/edit",
            get(admin_edit_form).post(admin_update),
        )
        .route("/blog/posts/:id/delete", post(admin_delete))
        .route("/blog/sites/new", get(admin_site_new_form))
        .route("/blog/sites", post(admin_site_create))
        .route(
            "/blog/sites/:id/edit",
            get(admin_site_edit_form).post(admin_site_update),
        )
        .route("/blog/sites/:id/delete", post(admin_site_delete))
        .with_state(AppState {
            db,
            events,
            site_name: Arc::new(site_name),
        })
}

#[derive(Deserialize, Default)]
struct SiteFilterQuery {
    site_id: Option<u64>,
}

#[derive(Template)]
#[template(path = "admin_list.html")]
struct AdminListPage {
    sites: Vec<AdminSiteCard>,
    posts: Vec<AdminPostRow>,
    total_posts: usize,
    active_site_id: Option<u64>,
    posts_heading: String,
}

struct AdminSiteCard {
    id: u64,
    name: String,
    hosts_display: String,
    description: String,
    accent_hex: &'static str,
    public_path: String,
    published_count: usize,
    draft_count: usize,
    is_default: bool,
}

struct AdminPostRow {
    id: u64,
    site_id: u64,
    site_name: String,
    title: String,
    slug: String,
    status_str: &'static str,
    updated_human: String,
}

async fn admin_list(
    State(state): State<AppState>,
    Query(query): Query<SiteFilterQuery>,
) -> Response {
    let all_sites = match state.db.all_blog_sites() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let all_posts = match state.db.all_blog_posts() {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mut site_names: HashMap<u64, String> = HashMap::new();
    site_names.insert(0, format!("{} (Default)", state.site_name));
    for s in &all_sites {
        site_names.insert(s.id, s.name.clone());
    }

    let mut post_site_ids: HashMap<u64, u64> = HashMap::new();
    let mut counts: HashMap<u64, (usize, usize)> = HashMap::new();
    for p in &all_posts {
        let sid = state.db.get_blog_post_site_id(p.id).unwrap_or(0);
        post_site_ids.insert(p.id, sid);
        let entry = counts.entry(sid).or_insert((0, 0));
        if p.status == PostStatus::Published {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    let mut site_cards = Vec::with_capacity(all_sites.len() + 1);
    let (def_pub, def_draft) = counts.get(&0).copied().unwrap_or((0, 0));
    site_cards.push(AdminSiteCard {
        id: 0,
        name: state.site_name.as_ref().clone(),
        hosts_display: String::new(),
        description: "Primary station blog (/blog/)".into(),
        accent_hex: theme_accent_hex("signal"),
        public_path: "/blog/".into(),
        published_count: def_pub,
        draft_count: def_draft,
        is_default: true,
    });
    for s in &all_sites {
        let (pub_c, draft_c) = counts.get(&s.id).copied().unwrap_or((0, 0));
        site_cards.push(AdminSiteCard {
            id: s.id,
            name: s.name.clone(),
            hosts_display: s.hosts.join(", "),
            description: s.description.clone(),
            accent_hex: theme_accent_hex(&s.theme),
            public_path: format!("/sites/{}/", s.slug),
            published_count: pub_c,
            draft_count: draft_c,
            is_default: false,
        });
    }

    let total_posts = all_posts.len();
    let active_site_id = query.site_id;
    let posts_heading = match active_site_id {
        Some(sid) => format!(
            "Posts in {}",
            site_names.get(&sid).cloned().unwrap_or_else(|| format!("Site #{sid}"))
        ),
        None => "All posts".into(),
    };

    let rows: Vec<AdminPostRow> = all_posts
        .into_iter()
        .filter(|p| {
            let sid = post_site_ids.get(&p.id).copied().unwrap_or(0);
            active_site_id.map(|f| f == sid).unwrap_or(true)
        })
        .map(|p| {
            let sid = post_site_ids.get(&p.id).copied().unwrap_or(0);
            AdminPostRow {
                id: p.id,
                site_id: sid,
                site_name: site_names
                    .get(&sid)
                    .cloned()
                    .unwrap_or_else(|| "Default".into()),
                title: p.title,
                slug: p.slug,
                status_str: p.status.as_str(),
                updated_human: format_unix(p.updated_at),
            }
        })
        .collect();

    render(AdminListPage {
        sites: site_cards,
        posts: rows,
        total_posts,
        active_site_id,
        posts_heading,
    })
}

// --- Blog Site Admin CRUD ---

#[derive(Template)]
#[template(path = "admin_site_edit.html")]
struct AdminSiteEditPage<'a> {
    heading: &'a str,
    form_action: String,
    delete_action: Option<String>,
    name: String,
    slug: String,
    slug_or_placeholder: String,
    hosts_text: String,
    description: String,
    theme: String,
    error: Option<&'a str>,
}

#[derive(Deserialize)]
struct SiteForm {
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    hosts: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    theme: String,
}

fn parse_hosts_input(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in raw.split(|c: char| c == ',' || c == '\n' || c == '\r' || c == ' ') {
        let h = normalize_host(token);
        if !h.is_empty() && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

async fn admin_site_new_form() -> Response {
    render(AdminSiteEditPage {
        heading: "Create blog",
        form_action: "/admin/blog/sites".into(),
        delete_action: None,
        name: String::new(),
        slug: String::new(),
        slug_or_placeholder: "field-notes".into(),
        hosts_text: String::new(),
        description: String::new(),
        theme: "signal".into(),
        error: None,
    })
}

async fn admin_site_create(State(state): State<AppState>, Form(form): Form<SiteForm>) -> Response {
    let now = unix_now();
    let id = match state.db.next_id("blog_site") {
        Ok(n) => n,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let name = form.name.trim().to_string();
    let slug = if form.slug.trim().is_empty() {
        slugify(&name)
    } else {
        slugify(&form.slug)
    };
    let hosts = parse_hosts_input(&form.hosts);
    let theme = match form.theme.as_str() {
        "phosphor" | "ink" | "signal" => form.theme.clone(),
        _ => "signal".into(),
    };
    let site = BlogSite {
        id,
        name: name.clone(),
        slug: slug.clone(),
        hosts: hosts.clone(),
        description: form.description.trim().to_string(),
        theme: theme.clone(),
        created_at: now,
        updated_at: now,
    };
    match state.db.upsert_blog_site(&site) {
        Ok(()) => Redirect::to(&format!("/admin/blog/posts?site_id={id}")).into_response(),
        Err(e) => {
            let msg = format!("Could not save blog: {e}");
            render(AdminSiteEditPage {
                heading: "Create blog",
                form_action: "/admin/blog/sites".into(),
                delete_action: None,
                name,
                slug: slug.clone(),
                slug_or_placeholder: if slug.is_empty() { "field-notes".into() } else { slug },
                hosts_text: form.hosts,
                description: form.description,
                theme,
                error: Some(&msg),
            })
        }
    }
}

async fn admin_site_edit_form(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let site = match state.db.get_blog_site(id) {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    render(AdminSiteEditPage {
        heading: "Manage blog",
        form_action: format!("/admin/blog/sites/{id}/edit"),
        delete_action: Some(format!("/admin/blog/sites/{id}/delete")),
        name: site.name,
        slug: site.slug.clone(),
        slug_or_placeholder: site.slug,
        hosts_text: site.hosts.join("\n"),
        description: site.description,
        theme: site.theme,
        error: None,
    })
}

async fn admin_site_update(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<SiteForm>,
) -> Response {
    let mut prev = match state.db.get_blog_site(id) {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let name = form.name.trim().to_string();
    let slug = if form.slug.trim().is_empty() {
        slugify(&name)
    } else {
        slugify(&form.slug)
    };
    let hosts = parse_hosts_input(&form.hosts);
    let theme = match form.theme.as_str() {
        "phosphor" | "ink" | "signal" => form.theme.clone(),
        _ => "signal".into(),
    };
    prev.name = name.clone();
    prev.slug = slug.clone();
    prev.hosts = hosts;
    prev.description = form.description.trim().to_string();
    prev.theme = theme.clone();
    prev.updated_at = unix_now();

    match state.db.upsert_blog_site(&prev) {
        Ok(()) => Redirect::to(&format!("/admin/blog/posts?site_id={id}")).into_response(),
        Err(e) => {
            let msg = format!("Could not update blog: {e}");
            render(AdminSiteEditPage {
                heading: "Manage blog",
                form_action: format!("/admin/blog/sites/{id}/edit"),
                delete_action: Some(format!("/admin/blog/sites/{id}/delete")),
                name,
                slug: slug.clone(),
                slug_or_placeholder: if slug.is_empty() { "field-notes".into() } else { slug },
                hosts_text: form.hosts,
                description: form.description,
                theme,
                error: Some(&msg),
            })
        }
    }
}

async fn admin_site_delete(State(state): State<AppState>, Path(id): Path<u64>) -> Response {
    let _ = state.db.delete_blog_site(id);
    Redirect::to("/admin/blog/posts").into_response()
}

// --- Post Admin CRUD ---

struct SiteOption {
    id: u64,
    label: String,
}

fn build_site_options(state: &AppState) -> Vec<SiteOption> {
    let mut opts = vec![SiteOption {
        id: 0,
        label: format!("{} (Default /blog/)", state.site_name),
    }];
    if let Ok(sites) = state.db.all_blog_sites() {
        for s in sites {
            let host_hint = if s.hosts.is_empty() {
                format!("/sites/{}/", s.slug)
            } else {
                s.hosts.join(", ")
            };
            opts.push(SiteOption {
                id: s.id,
                label: format!("{} ({})", s.name, host_hint),
            });
        }
    }
    opts
}

#[derive(Template)]
#[template(path = "admin_edit.html")]
struct AdminEditPage<'a> {
    heading: &'a str,
    form_action: String,
    site_options: Vec<SiteOption>,
    selected_site_id: u64,
    title: String,
    slug: String,
    slug_or_placeholder: String,
    content: String,
    status_str: &'static str,
    error: Option<&'a str>,
}

async fn admin_new_form(
    State(state): State<AppState>,
    Query(query): Query<SiteFilterQuery>,
) -> Response {
    render(AdminEditPage {
        heading: "New post",
        form_action: "/admin/blog/posts".into(),
        site_options: build_site_options(&state),
        selected_site_id: query.site_id.unwrap_or(0),
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
    let site_id = state.db.get_blog_post_site_id(id).unwrap_or(0);
    render(AdminEditPage {
        heading: "Edit post",
        form_action: format!("/admin/blog/posts/{}/edit", id),
        site_options: build_site_options(&state),
        selected_site_id: site_id,
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
    #[serde(default)]
    site_id: u64,
    title: String,
    #[serde(default)]
    slug: String,
    content: String,
    status: String,
}

async fn admin_create(State(state): State<AppState>, Form(form): Form<PostForm>) -> Response {
    let now = unix_now();
    let id = state.db.next_id("blog_post").unwrap_or(now);
    let status = PostStatus::from_form(&form.status);
    let title = form.title.trim().to_string();
    let slug = if form.slug.trim().is_empty() {
        slugify(&title)
    } else {
        slugify(&form.slug)
    };
    let post = BlogPost {
        id,
        slug,
        title,
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
    match state.db.upsert_blog_post_for_site(form.site_id, &post) {
        Ok(()) => {
            if status == PostStatus::Published {
                state
                    .events
                    .publish(Event::BlogPostPublished { id: post.id });
            }
            let dest = if form.site_id > 0 {
                format!("/admin/blog/posts?site_id={}", form.site_id)
            } else {
                "/admin/blog/posts".into()
            };
            Redirect::to(&dest).into_response()
        }
        Err(e) => {
            let msg = format!("Could not save: {e}");
            render(AdminEditPage {
                heading: "New post",
                form_action: "/admin/blog/posts".into(),
                site_options: build_site_options(&state),
                selected_site_id: form.site_id,
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

    let title = form.title.trim().to_string();
    let slug = if form.slug.trim().is_empty() {
        slugify(&title)
    } else {
        slugify(&form.slug)
    };
    prev.title = title;
    prev.slug = slug;
    prev.content = form.content;
    prev.status = new_status;
    prev.updated_at = now;
    if new_status == PostStatus::Published && !was_already_published {
        prev.published_at = Some(now);
    } else if new_status == PostStatus::Draft {
        prev.published_at = None;
    }

    match state.db.upsert_blog_post_for_site(form.site_id, &prev) {
        Ok(()) => {
            if new_status == PostStatus::Published && !was_already_published {
                state.events.publish(Event::BlogPostPublished { id });
            }
            let dest = if form.site_id > 0 {
                format!("/admin/blog/posts?site_id={}", form.site_id)
            } else {
                "/admin/blog/posts".into()
            };
            Redirect::to(&dest).into_response()
        }
        Err(e) => {
            let msg = format!("Could not save: {e}");
            render(AdminEditPage {
                heading: "Edit post",
                form_action: format!("/admin/blog/posts/{id}/edit"),
                site_options: build_site_options(&state),
                selected_site_id: form.site_id,
                title: prev.title.clone(),
                slug: prev.slug.clone(),
                slug_or_placeholder: prev.slug,
                content: prev.content,
                status_str: prev.status.as_str(),
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

fn render_post_html(input: &str) -> String {
    ammonia::Builder::default()
        .add_tag_attributes("div", &["id", "class"])
        .add_tag_attributes("sup", &["class"])
        .clean(&render_markdown(input))
        .to_string()
}

fn markdown_excerpt(input: &str, max_chars: usize) -> String {
    use pulldown_cmark::{Event, Parser, TagEnd};
    let mut out = String::new();
    for ev in Parser::new(input) {
        match ev {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Item) => out.push(' '),
            _ => {}
        }
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
    fn slugify_generates_clean_slugs() {
        assert_eq!(slugify("The First Cold Morning!"), "the-first-cold-morning");
        assert_eq!(slugify("  Hello---World  "), "hello-world");
    }

    #[test]
    fn parse_hosts_splits_and_normalizes() {
        let hosts = parse_hosts_input("HTTPS://Notes.Example.COM, studio.example.com\nnotes.example.com:443");
        assert_eq!(hosts, vec!["notes.example.com", "studio.example.com"]);
    }

    #[test]
    fn public_pages_render_the_configured_site_name() {
        let html = PublicIndex {
            site_name: "Radio Free Harbor".into(),
            site_description: "Latest posts from Radio Free Harbor.".into(),
            accent_hex: theme_accent_hex("signal"),
            home_href: "/".into(),
            post_base_href: "/blog/".into(),
            standalone_site: false,
            posts: Vec::new(),
        }
        .render()
        .unwrap();
        assert!(html.contains("<title>Blog — Radio Free Harbor</title>"));
        assert!(html.contains(">Radio Free Harbor</a>"));
        assert!(html.contains("Latest posts from Radio Free Harbor."));
        assert!(html.contains("Powered by Nuntius"));

        let standalone = PublicIndex {
            site_name: "Field Notes".into(),
            site_description: "Observations from the field.".into(),
            accent_hex: theme_accent_hex("phosphor"),
            home_href: "/".into(),
            post_base_href: "/posts/".into(),
            standalone_site: true,
            posts: Vec::new(),
        }
        .render()
        .unwrap();
        assert!(standalone.contains("<title>Field Notes</title>"));
        assert!(!standalone.contains("Live radio"));
        assert!(standalone.contains("Observations from the field."));

        let post = PublicPost {
            site_name: "Radio Free Harbor".into(),
            accent_hex: theme_accent_hex("signal"),
            home_href: "/".into(),
            back_href: "/blog/".into(),
            standalone_site: false,
            title: "Hello".into(),
            rendered_html: "<p>hi</p>".into(),
            published_human: "Jan 1, 2026".into(),
            meta_description: "hi".into(),
        }
        .render()
        .unwrap();
        assert!(post.contains(">Radio Free Harbor</a>"));
        assert!(post.contains("Powered by Nuntius"));
    }

    #[test]
    fn script_tags_are_stripped_from_rendered_posts() {
        let html =
            render_post_html("Intro paragraph.\n\n<script>alert('xss')</script>\n\n**bold** end");
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert("));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn event_handlers_and_js_urls_are_stripped() {
        let html = render_post_html(
            "<img src=\"x\" onerror=\"alert(1)\">\n\n<a href=\"javascript:alert(2)\">click</a>",
        );
        assert!(!html.contains("onerror"));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn footnote_anchors_survive_sanitization() {
        let html = render_post_html("A claim.[^1]\n\n[^1]: The footnote text.");
        assert!(html.contains("href=\"#1\""));
        assert!(html.contains("id=\"1\""));
        let attack =
            render_post_html("<div id=\"x\" onclick=\"alert(1)\"><script>bad()</script></div>");
        assert!(!attack.contains("onclick"));
        assert!(!attack.contains("<script"));
        assert!(attack.contains("id=\"x\""));
    }
}
