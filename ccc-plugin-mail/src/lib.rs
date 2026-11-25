//! Native IMAP + SMTP Mail plugin for Nuntius Creator Command Center.
//!
//! Connects to any standard IMAP/SMTP provider (IONOS Mail, Fastmail, Gmail
//! App Password, etc.) so solo media operators can read, reply to, and send
//! email right inside `/admin/mail` without hosting a full MTA on the VPS.

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use ccc_core::{
    AdminNavEntry, AppContext, CccConfig, Plugin, PluginError, PluginLifecycle, PluginStatus,
};
use ccc_plugin_utils::render;
use lettre::{
    message::Mailbox,
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use mailparse::{parse_mail, MailHeaderMap};
use serde::Deserialize;
use std::any::Any;
use std::sync::Arc;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MailSettings {
    #[serde(default)]
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub username: String,
    /// Preferred: name of an environment variable holding the mailbox password.
    #[serde(default)]
    pub password_env: String,
    /// Optional fallback for local private configs if `password_env` is unset.
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub from_name: String,
    #[serde(default)]
    pub from_address: String,
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    587
}

impl MailSettings {
    fn resolved_password(&self) -> String {
        if !self.password_env.trim().is_empty() {
            if let Ok(val) = std::env::var(self.password_env.trim()) {
                if !val.is_empty() {
                    return val;
                }
            }
        }
        self.password.clone()
    }

    pub fn is_configured(&self) -> bool {
        !self.imap_host.trim().is_empty()
            && !self.smtp_host.trim().is_empty()
            && !self.username.trim().is_empty()
            && !self.resolved_password().is_empty()
    }

    fn effective_from_address(&self) -> String {
        if self.from_address.trim().is_empty() {
            self.username.trim().to_string()
        } else {
            self.from_address.trim().to_string()
        }
    }
}

pub struct MailPlugin {
    settings: Arc<MailSettings>,
}

impl MailPlugin {
    pub fn from_config(cfg: &CccConfig) -> Result<Self, PluginError> {
        let settings: MailSettings = cfg
            .plugin
            .get("mail")
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default();
        Ok(Self {
            settings: Arc::new(settings),
        })
    }
}

impl Plugin for MailPlugin {
    fn name(&self) -> &'static str {
        "Mail"
    }
    fn version(&self) -> &'static str {
        "0.1.0"
    }
    fn id(&self) -> String {
        "mail".into()
    }
    fn kind(&self) -> &'static str {
        "COMMUNICATION"
    }
    fn icon(&self) -> &'static str {
        "icon-newsletter"
    }
    fn admin_url(&self) -> String {
        "/admin/mail".into()
    }

    fn status(&self, _ctx: &AppContext) -> PluginStatus {
        let configured = self.settings.is_configured();
        PluginStatus {
            state: if configured {
                PluginLifecycle::Configured
            } else {
                PluginLifecycle::Unconfigured
            },
            badge: None,
            detail: serde_json::json!({
                "configured": configured,
                "imap_host": self.settings.imap_host,
                "smtp_host": self.settings.smtp_host,
                "account": self.settings.effective_from_address(),
            }),
        }
    }

    fn init(&self) -> Result<(), PluginError> {
        println!(
            "Initializing Mail Plugin (IMAP/SMTP configured={}).",
            self.settings.is_configured()
        );
        Ok(())
    }

    fn admin_routes(&self, _ctx: &AppContext) -> Option<Router> {
        Some(
            Router::new()
                .route("/mail", get(admin_inbox))
                .route("/mail/message/:uid", get(admin_message))
                .route("/mail/compose", get(admin_compose))
                .route("/mail/send", post(admin_send))
                .with_state(self.settings.clone()),
        )
    }

    fn admin_nav(&self, _ctx: &AppContext) -> Vec<AdminNavEntry> {
        vec![AdminNavEntry {
            label: "Mail",
            href: "/admin/mail".into(),
            icon: "✉️",
            sort: 230,
        }]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ============================================================================
// Admin handlers
// ============================================================================

#[derive(Deserialize, Default)]
struct InboxQuery {
    folder: Option<String>,
    sent: Option<u8>,
}

struct MailRow {
    uid: u32,
    from: String,
    subject: String,
    date: String,
    unread: bool,
}

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxPage {
    configured: bool,
    account_label: String,
    folder: String,
    messages: Vec<MailRow>,
    flash: Option<String>,
    error: Option<String>,
}

fn sanitize_folder(raw: Option<String>) -> String {
    let f = raw.unwrap_or_else(|| "INBOX".into());
    let cleaned: String = f
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '/'))
        .take(64)
        .collect();
    if cleaned.trim().is_empty() {
        "INBOX".into()
    } else {
        cleaned
    }
}

async fn admin_inbox(
    State(settings): State<Arc<MailSettings>>,
    Query(query): Query<InboxQuery>,
) -> Response {
    let folder = sanitize_folder(query.folder);
    let flash = if query.sent == Some(1) {
        Some("Email sent successfully.".into())
    } else {
        None
    };

    if !settings.is_configured() {
        return render(InboxPage {
            configured: false,
            account_label: String::new(),
            folder,
            messages: Vec::new(),
            flash,
            error: None,
        });
    }

    let account_label = settings.effective_from_address();
    let cfg = settings.clone();
    let folder_clone = folder.clone();
    let fetched = tokio::task::spawn_blocking(move || fetch_folder_messages(&cfg, &folder_clone, 30))
        .await;

    match fetched {
        Ok(Ok(messages)) => render(InboxPage {
            configured: true,
            account_label,
            folder,
            messages,
            flash,
            error: None,
        }),
        Ok(Err(e)) => render(InboxPage {
            configured: true,
            account_label,
            folder,
            messages: Vec::new(),
            flash,
            error: Some(format!("IMAP error: {e}")),
        }),
        Err(e) => render(InboxPage {
            configured: true,
            account_label,
            folder,
            messages: Vec::new(),
            flash,
            error: Some(format!("Task error: {e}")),
        }),
    }
}

#[derive(Deserialize, Default)]
struct MessageQuery {
    folder: Option<String>,
}

#[derive(Template)]
#[template(path = "message.html")]
struct MessagePage {
    folder: String,
    subject: String,
    from: String,
    to: String,
    date: String,
    is_html: bool,
    body_html: String,
    body_text: String,
    reply_to_encoded: String,
    reply_subject_encoded: String,
}

async fn admin_message(
    State(settings): State<Arc<MailSettings>>,
    Path(uid): Path<u32>,
    Query(query): Query<MessageQuery>,
) -> Response {
    if !settings.is_configured() {
        return Redirect::to("/admin/mail").into_response();
    }
    let folder = sanitize_folder(query.folder);
    let cfg = settings.clone();
    let folder_clone = folder.clone();
    let fetched = tokio::task::spawn_blocking(move || fetch_single_message(&cfg, &folder_clone, uid))
        .await;

    match fetched {
        Ok(Ok(Some(msg))) => {
            let reply_to = extract_email_addr(&msg.from);
            let reply_subj = if msg.subject.to_ascii_lowercase().starts_with("re:") {
                msg.subject.clone()
            } else {
                format!("Re: {}", msg.subject)
            };
            render(MessagePage {
                folder,
                subject: msg.subject,
                from: msg.from,
                to: msg.to,
                date: msg.date,
                is_html: msg.is_html,
                body_html: msg.body_html,
                body_text: msg.body_text,
                reply_to_encoded: url_encode_component(&reply_to),
                reply_subject_encoded: url_encode_component(&reply_subj),
            })
        }
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, format!("IMAP error: {e}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Task error: {e}")).into_response(),
    }
}

#[derive(Deserialize, Default)]
struct ComposeQuery {
    to: Option<String>,
    subject: Option<String>,
}

#[derive(Template)]
#[template(path = "compose.html")]
struct ComposePage {
    to: String,
    subject: String,
    body: String,
    from_address: String,
    error: Option<String>,
}

async fn admin_compose(
    State(settings): State<Arc<MailSettings>>,
    Query(query): Query<ComposeQuery>,
) -> Response {
    if !settings.is_configured() {
        return Redirect::to("/admin/mail").into_response();
    }
    render(ComposePage {
        to: query.to.unwrap_or_default(),
        subject: query.subject.unwrap_or_default(),
        body: String::new(),
        from_address: settings.effective_from_address(),
        error: None,
    })
}

#[derive(Deserialize)]
struct SendForm {
    to: String,
    subject: String,
    body: String,
}

async fn admin_send(
    State(settings): State<Arc<MailSettings>>,
    Form(form): Form<SendForm>,
) -> Response {
    if !settings.is_configured() {
        return Redirect::to("/admin/mail").into_response();
    }
    match send_smtp_email(&settings, &form.to, &form.subject, &form.body).await {
        Ok(()) => Redirect::to("/admin/mail?sent=1").into_response(),
        Err(e) => render(ComposePage {
            to: form.to,
            subject: form.subject,
            body: form.body,
            from_address: settings.effective_from_address(),
            error: Some(format!("Could not send email: {e}")),
        }),
    }
}

// ============================================================================
// IMAP & SMTP transport helpers
// ============================================================================

struct FullMessage {
    subject: String,
    from: String,
    to: String,
    date: String,
    is_html: bool,
    body_html: String,
    body_text: String,
}

fn connect_imap(
    settings: &MailSettings,
) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>, String> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| format!("TLS init failed: {e}"))?;
    let client = imap::connect(
        (settings.imap_host.as_str(), settings.imap_port),
        settings.imap_host.as_str(),
        &tls,
    )
    .map_err(|e| format!("connect failed: {e}"))?;
    client
        .login(settings.username.trim(), settings.resolved_password())
        .map_err(|(e, _)| format!("login failed: {e}"))
}

fn fetch_folder_messages(
    settings: &MailSettings,
    folder: &str,
    limit: u32,
) -> Result<Vec<MailRow>, String> {
    let mut session = connect_imap(settings)?;
    let mailbox = session
        .examine(folder)
        .map_err(|e| format!("select folder '{folder}': {e}"))?;
    let total = mailbox.exists;
    if total == 0 {
        let _ = session.logout();
        return Ok(Vec::new());
    }
    let start = total.saturating_sub(limit.saturating_sub(1)).max(1);
    let seq_range = format!("{start}:{total}");
    let fetches = session
        .fetch(&seq_range, "(UID FLAGS ENVELOPE)")
        .map_err(|e| format!("fetch envelopes: {e}"))?;

    let mut rows = Vec::new();
    for msg in fetches.iter() {
        let uid = match msg.uid {
            Some(u) => u,
            None => continue,
        };
        let unread = !msg
            .flags()
            .iter()
            .any(|f| matches!(f, imap::types::Flag::Seen));
        let (from, subject, date) = if let Some(env) = msg.envelope() {
            let subj = env
                .subject
                .as_ref()
                .map(|b| decode_header_bytes(b))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(no subject)".into());
            let from_str = env
                .from
                .as_ref()
                .and_then(|addrs| addrs.first())
                .map(format_imap_address)
                .unwrap_or_else(|| "(unknown)".into());
            let date_str = env
                .date
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            (from_str, subj, date_str)
        } else {
            ("(unknown)".into(), "(no subject)".into(), String::new())
        };
        rows.push(MailRow {
            uid,
            from,
            subject,
            date,
            unread,
        });
    }
    let _ = session.logout();
    rows.reverse();
    Ok(rows)
}

fn fetch_single_message(
    settings: &MailSettings,
    folder: &str,
    uid: u32,
) -> Result<Option<FullMessage>, String> {
    let mut session = connect_imap(settings)?;
    session
        .select(folder)
        .map_err(|e| format!("select folder '{folder}': {e}"))?;
    let fetches = session
        .uid_fetch(uid.to_string(), "RFC822")
        .map_err(|e| format!("fetch message {uid}: {e}"))?;
    let raw = match fetches.iter().next().and_then(|m| m.body()) {
        Some(bytes) => bytes.to_vec(),
        None => {
            let _ = session.logout();
            return Ok(None);
        }
    };
    let _ = session.logout();
    parse_full_message(&raw).map(Some)
}

fn parse_full_message(raw: &[u8]) -> Result<FullMessage, String> {
    let parsed = parse_mail(raw).map_err(|e| format!("MIME parse failed: {e}"))?;
    let subject = parsed
        .headers
        .get_first_value("Subject")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "(no subject)".into());
    let from = parsed
        .headers
        .get_first_value("From")
        .unwrap_or_else(|| "(unknown)".into());
    let to = parsed.headers.get_first_value("To").unwrap_or_default();
    let date = parsed.headers.get_first_value("Date").unwrap_or_default();

    let mut html_body = None;
    let mut text_body = None;
    collect_bodies(&parsed, &mut html_body, &mut text_body);

    if let Some(html) = html_body {
        Ok(FullMessage {
            subject,
            from,
            to,
            date,
            is_html: true,
            body_html: ammonia::clean(&html),
            body_text: String::new(),
        })
    } else {
        Ok(FullMessage {
            subject,
            from,
            to,
            date,
            is_html: false,
            body_html: String::new(),
            body_text: text_body.unwrap_or_default(),
        })
    }
}

fn collect_bodies(
    part: &mailparse::ParsedMail<'_>,
    html_out: &mut Option<String>,
    text_out: &mut Option<String>,
) {
    if part.subparts.is_empty() {
        let ct = part.ctype.mimetype.to_ascii_lowercase();
        if ct == "text/html" && html_out.is_none() {
            if let Ok(b) = part.get_body() {
                *html_out = Some(b);
            }
        } else if ct == "text/plain" && text_out.is_none() {
            if let Ok(b) = part.get_body() {
                *text_out = Some(b);
            }
        }
    } else {
        for sub in &part.subparts {
            collect_bodies(sub, html_out, text_out);
        }
    }
}

async fn send_smtp_email(
    settings: &MailSettings,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    let from_addr = settings.effective_from_address();
    let from_box: Mailbox = if settings.from_name.trim().is_empty() {
        from_addr
            .parse()
            .map_err(|e| format!("invalid from address: {e}"))?
    } else {
        format!("{} <{}>", settings.from_name.trim(), from_addr)
            .parse()
            .map_err(|e| format!("invalid from mailbox: {e}"))?
    };
    let to_box: Mailbox = to
        .trim()
        .parse()
        .map_err(|e| format!("invalid recipient address: {e}"))?;

    let email = Message::builder()
        .from(from_box)
        .to(to_box)
        .subject(subject.trim())
        .body(body.to_string())
        .map_err(|e| format!("build email: {e}"))?;

    let creds = Credentials::new(
        settings.username.trim().to_string(),
        settings.resolved_password(),
    );
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(settings.smtp_host.trim())
        .map_err(|e| format!("SMTP relay config: {e}"))?
        .port(settings.smtp_port)
        .credentials(creds)
        .build();

    mailer
        .send(email)
        .await
        .map_err(|e| format!("SMTP send failed: {e}"))?;
    Ok(())
}

fn decode_header_bytes(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    mailparse::parse_header(format!("X: {raw}").as_bytes())
        .map(|(h, _)| h.get_value())
        .unwrap_or_else(|_| raw.into_owned())
}

fn format_imap_address(addr: &imap_proto::Address<'_>) -> String {
    let mailbox = addr
        .mailbox
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let host = addr
        .host
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let email = if !mailbox.is_empty() && !host.is_empty() {
        format!("{mailbox}@{host}")
    } else {
        mailbox
    };
    if let Some(name) = addr.name.as_ref() {
        let decoded = decode_header_bytes(name);
        if !decoded.trim().is_empty() {
            return format!("{decoded} <{email}>");
        }
    }
    email
}

fn extract_email_addr(raw: &str) -> String {
    if let (Some(start), Some(end)) = (raw.find('<'), raw.rfind('>')) {
        if start < end {
            return raw[start + 1..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

fn url_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mime_sanitizes_html_email() {
        let raw = concat!(
            "From: Alice <alice@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Hello\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<p>Safe</p><script>alert(1)</script>"
        );
        let msg = parse_full_message(raw.as_bytes()).unwrap();
        assert!(msg.is_html);
        assert!(msg.body_html.contains("<p>Safe</p>"));
        assert!(!msg.body_html.contains("<script"));
        assert_eq!(extract_email_addr(&msg.from), "alice@example.com");
    }

    #[test]
    fn folder_name_is_sanitized() {
        assert_eq!(sanitize_folder(Some("INBOX".into())), "INBOX");
        assert_eq!(sanitize_folder(Some("Sent\r\nLOGOUT".into())), "SentLOGOUT");
        assert_eq!(sanitize_folder(None), "INBOX");
    }
}
