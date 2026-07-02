//! The plugin dispatch table.
//!
//! Maps the kebab-case plugin identifiers used in `ccc.toml` to the concrete
//! constructors. Only plugins listed in `[plugins].enabled` are constructed —
//! everything else stays absent from memory.
//!
//! When you add a new plugin to the workspace, add a match arm here.

use ccc_core::{AppContext, CccConfig, PluginError, PluginManager};

/// Construct and register a plugin by its config identifier.
///
/// Returns `Err(unknown)` if the name doesn't map to any in-tree plugin.
pub fn register_by_name(
    manager: &mut PluginManager,
    name: &str,
    cfg: &CccConfig,
    _ctx: &AppContext,
) -> Result<(), DispatchError> {
    match name {
        "cms" => {
            let plugin = ccc_cms::CmsPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "radio-engine" => {
            let plugin = ccc_radio_engine::RadioEnginePlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "blog" => {
            manager.register(ccc_plugin_blog::StaticBlogPlugin)?;
        }
        "podcast" => {
            manager.register(ccc_plugin_podcast::PodcastPlugin::from_config(cfg))?;
        }
        "video-podcast" => {
            manager.register(ccc_plugin_video_podcast::VideoPodcastPlugin::from_config(
                cfg,
            ))?;
        }
        "store" => {
            manager.register(ccc_plugin_store::PluginStorePlugin)?;
        }
        "drop" => {
            manager.register(ccc_plugin_drop::DropPlugin)?;
        }
        "call-studio" => {
            manager.register(ccc_plugin_call_studio::CallStudioPlugin)?;
        }
        "activitypub" => {
            let plugin = ccc_plugin_activitypub::CccPluginActivitypubPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "ads" => {
            manager.register(ccc_plugin_ads::CccPluginAdsPlugin)?;
        }
        "analytics" => {
            manager.register(ccc_plugin_analytics::CccPluginAnalyticsPlugin::new())?;
        }
        "bridge" => {
            manager.register(ccc_plugin_bridge::BridgePlugin::default())?;
        }
        "newsletter" => {
            let plugin = ccc_plugin_newsletter::CccPluginNewsletterPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "s3" => {
            let plugin = ccc_plugin_s3::CccPluginS3Plugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "stream-watch" => {
            let plugin = ccc_plugin_stream_watch::StreamWatchPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "live-chat" => {
            let plugin = ccc_plugin_live_chat::LiveChatPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "seo" => {
            let plugin = ccc_plugin_seo::CccPluginSeoPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "theme-dark-matter" => {
            manager.register(ccc_plugin_theme_dark_matter::CccPluginThemeDarkMatterPlugin)?;
        }
        "whisper" => {
            let plugin = ccc_plugin_whisper::CccPluginWhisperPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        other => return Err(DispatchError::Unknown(other.to_string())),
    }
    Ok(())
}

/// Every identifier `register_by_name` understands. Keep in sync with the match arms above.
pub fn known_plugins() -> &'static [&'static str] {
    &[
        "cms",
        "radio-engine",
        "blog",
        "podcast",
        "video-podcast",
        "store",
        "drop",
        "call-studio",
        "activitypub",
        "ads",
        "analytics",
        "bridge",
        "newsletter",
        "s3",
        "seo",
        "stream-watch",
        "live-chat",
        "theme-dark-matter",
        "whisper",
    ]
}

/// Pure validation for `[plugins].enabled`: returns every configured name
/// this build does not recognize, in config order, deduplicated. An empty
/// result means every name maps to a dispatch-table arm.
///
/// The CLI calls this before opening the database or binding the listener,
/// so a typo'd plugin name fails startup loudly instead of silently
/// dropping a feature the operator asked for.
pub fn unknown_plugin_names(enabled: &[String]) -> Vec<String> {
    let mut unknown: Vec<String> = Vec::new();
    for name in enabled {
        if !known_plugins().contains(&name.as_str()) && !unknown.contains(name) {
            unknown.push(name.clone());
        }
    }
    unknown
}

#[derive(Debug)]
pub enum DispatchError {
    Unknown(String),
    Plugin(PluginError),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Unknown(name) => {
                write!(f, "unknown plugin '{}': not in dispatch table", name)
            }
            DispatchError::Plugin(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for DispatchError {}

impl From<PluginError> for DispatchError {
    fn from(e: PluginError) -> Self {
        DispatchError::Plugin(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_enabled_list_is_valid() {
        assert!(unknown_plugin_names(&[]).is_empty());
    }

    #[test]
    fn every_known_plugin_passes_validation() {
        let enabled = owned(known_plugins());
        assert!(unknown_plugin_names(&enabled).is_empty());
    }

    #[test]
    fn unknown_names_reported_in_config_order() {
        let enabled = owned(&["cms", "blgo", "radio-engine", "poodcast"]);
        assert_eq!(unknown_plugin_names(&enabled), owned(&["blgo", "poodcast"]));
    }

    #[test]
    fn duplicate_unknown_names_reported_once() {
        let enabled = owned(&["blgo", "blgo", "blgo"]);
        assert_eq!(unknown_plugin_names(&enabled), owned(&["blgo"]));
    }

    #[test]
    fn identifiers_are_case_sensitive() {
        // Config identifiers are exact kebab-case strings; "CMS" is a typo.
        let enabled = owned(&["CMS"]);
        assert_eq!(unknown_plugin_names(&enabled), owned(&["CMS"]));
    }
}
