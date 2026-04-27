//! Askama template structs.
//!
//! Each `#[derive(Template)]` corresponds to a `.html` file under `templates/`.
//! Common parts (the three-column shell, head, nav) live in `_layout.html`
//! and are inherited via `{% extends "_layout.html" %}`.

use askama::Template;

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

/// Authenticated Nuntius admin shell. Renders the SPA chrome; all
/// plugin data hydrates client-side from `/admin/api/plugins` and
/// `/admin/api/plugins/:id/status`.
#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub admin_username: String,
}
