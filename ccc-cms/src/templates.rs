//! Askama template structs.
//!
//! Each `#[derive(Template)]` corresponds to a `.html` file under `templates/`.
//! Common parts (the three-column shell, head, nav) live in `_layout.html`
//! and are inherited via `{% extends "_layout.html" %}`.

use askama::Template;

/// Sidebar navigation entry. Plugins contribute these via the dashboard
/// (TODO: wire plugin contribution later).
pub struct NavSection {
    pub label: &'static str,
    pub href: String,
    pub icon: &'static str,
}

/// First-run setup wizard.
#[derive(Template)]
#[template(path = "setup.html")]
pub struct SetupPage<'a> {
    pub error: Option<&'a str>,
}

/// Login form.
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage<'a> {
    pub error: Option<&'a str>,
    pub username: &'a str,
}

/// Authenticated dashboard landing page.
#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub admin_username: String,
    pub nav: Vec<NavSection>,
    pub plugin_count: usize,
    pub plugin_names: Vec<String>,
}
