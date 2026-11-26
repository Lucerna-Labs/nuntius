//! Pluggable `AssistantProvider` trait for `ccc-plugin-agents`.
//!
//! `SuiteAssistantProvider` handles built-in Nuntius suite inspection and
//! approval-gated draft proposals locally with zero external network calls.
//! To connect an external API-hosted model, implement `AssistantProvider`
//! (or extend `SuiteAssistantProvider::respond`) with your HTTP client.

use ccc_database::{AgentBot, AgentMessage, AgentProposal, CccDB, PostStatus, ProposalStatus};
use ccc_plugin_utils::unix_now;
use std::future::Future;
use std::pin::Pin;

pub struct AssistantRequest<'a> {
    pub db: &'a CccDB,
    pub site_name: &'a str,
    pub bot: &'a AgentBot,
    pub current_path: &'a str,
    pub history: &'a [AgentMessage],
    pub user_message: &'a str,
}

pub struct AssistantReply {
    pub content: String,
    pub proposal: Option<AgentProposal>,
}

pub trait AssistantProvider: Send + Sync {
    fn respond<'a>(
        &'a self,
        req: AssistantRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<AssistantReply, String>> + Send + 'a>>;
}

/// Built-in suite command & approval-proposal provider.
///
/// Supports immediate Nuntius management commands (`status`, `blogs`,
/// `draft <title> | <body>`, `help`) and provides a clean extension point
/// for wiring an external API model backend.
pub struct SuiteAssistantProvider;

impl AssistantProvider for SuiteAssistantProvider {
    fn respond<'a>(
        &'a self,
        req: AssistantRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<AssistantReply, String>> + Send + 'a>> {
        let site_name = req.site_name.to_string();
        let bot = req.bot.clone();
        let current_path = req.current_path.to_string();
        let input = req.user_message.trim().to_string();

        let sites = req.db.all_blog_sites().unwrap_or_default();
        let posts = req.db.all_blog_posts().unwrap_or_default();
        let pending = req.db.pending_agent_proposals_count().unwrap_or(0);

        Box::pin(async move {
            let lower = input.to_ascii_lowercase();

            if lower == "help" || lower == "/help" {
                return Ok(AssistantReply {
                    content: format!(
                        "**{}** ({}) — built-in suite commands:\n\n\
                         - `status` — live overview of blogs, posts, pending approvals, and active tab\n\
                         - `blogs` — list all configured multi-domain blogs\n\
                         - `draft Title | Markdown body` — queue an approval-gated blog draft\n\n\
                         *To connect an external API model, implement `AssistantProvider` in `ccc-plugin-agents/src/provider.rs`.*",
                        bot.name, bot.role
                    ),
                    proposal: None,
                });
            }

            if lower == "status" || lower == "/status" || lower.contains("suite status") {
                let published = posts
                    .iter()
                    .filter(|p| p.status == PostStatus::Published)
                    .count();
                let drafts = posts.len().saturating_sub(published);
                return Ok(AssistantReply {
                    content: format!(
                        "**Nuntius Suite Status ({site_name})**\n\n\
                         - Active tab: `{current_path}`\n\
                         - Blogs: **{}** (1 default + {} custom domain/subdomain sites)\n\
                         - Posts: **{published}** published, **{drafts}** drafts\n\
                         - Pending approvals: **{pending}**",
                        sites.len() + 1,
                        sites.len()
                    ),
                    proposal: None,
                });
            }

            if lower == "blogs" || lower == "/blogs" || lower == "list blogs" {
                let mut lines = vec![format!("- **{}** (Default `/blog/`)", site_name)];
                for s in &sites {
                    let host_str = if s.hosts.is_empty() {
                        format!("/sites/{}/", s.slug)
                    } else {
                        s.hosts.join(", ")
                    };
                    lines.push(format!("- **{}** (`{}`)", s.name, host_str));
                }
                return Ok(AssistantReply {
                    content: format!("**Configured Blogs**\n\n{}", lines.join("\n")),
                    proposal: None,
                });
            }

            if let Some(rest) = input
                .strip_prefix("draft ")
                .or_else(|| input.strip_prefix("Draft "))
                .or_else(|| input.strip_prefix("draft:"))
                .or_else(|| input.strip_prefix("Draft:"))
            {
                let rest = rest.trim();
                let (title, body) = match rest.split_once('|') {
                    Some((t, b)) => (t.trim().to_string(), b.trim().to_string()),
                    None => (
                        rest.to_string(),
                        format!("# {rest}\n\nDrafted from Nuntius AI Command Dock."),
                    ),
                };
                if !title.is_empty() {
                    let slug = slugify(&title);
                    let prop = AgentProposal {
                        id: 0,
                        bot_id: bot.id,
                        kind: "draft_blog_post".into(),
                        site_id: bot.default_blog_site_id,
                        title: title.clone(),
                        slug: slug.clone(),
                        content: body,
                        status: ProposalStatus::Pending,
                        created_at: unix_now(),
                    };
                    return Ok(AssistantReply {
                        content: format!(
                            "Queued draft **{}** (`{}`) for your approval below.",
                            title, slug
                        ),
                        proposal: Some(prop),
                    });
                }
            }

            // Default response until an external API backend is plugged into `AssistantProvider`.
            Ok(AssistantReply {
                content: format!(
                    "I'm **{}** watching `{current_path}`. Right now I'm running with the built-in local suite handler (`status`, `blogs`, `draft Title | Body`, `help`).\n\n\
                     Plug your external API client into `ccc-plugin-agents/src/provider.rs` (`AssistantProvider::respond`) to enable full conversational AI for `{}` / `{}`.",
                    bot.name, bot.provider, bot.model
                ),
                proposal: None,
            })
        })
    }
}

pub fn slugify(input: &str) -> String {
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
