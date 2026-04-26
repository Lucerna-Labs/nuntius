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
            manager.register(ccc_plugin_video_podcast::VideoPodcastPlugin::from_config(cfg))?;
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
        "newsletter" => {
            let plugin = ccc_plugin_newsletter::CccPluginNewsletterPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "patreon" => {
            let plugin = ccc_plugin_patreon::CccPluginPatreonPlugin::from_config(cfg)?;
            manager.register(plugin)?;
        }
        "s3" => {
            let plugin = ccc_plugin_s3::CccPluginS3Plugin::from_config(cfg)?;
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
        "newsletter",
        "patreon",
        "s3",
        "seo",
        "theme-dark-matter",
        "whisper",
    ]
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
