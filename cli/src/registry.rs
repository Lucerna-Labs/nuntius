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
        "blog" => {
            manager.register(ccc_plugin_blog::StaticBlogPlugin::default())?;
        }
        "podcast" => {
            // Plugin internally uses ctx.db at routes() time; db_path is informational.
            let db_path = cfg
                .database
                .path
                .to_string_lossy()
                .into_owned();
            manager.register(ccc_plugin_podcast::PodcastPlugin::new(&db_path))?;
        }
        "video-podcast" => {
            // Per-plugin config under [plugin.video-podcast].
            let hls_dir = cfg
                .plugin
                .get("video-podcast")
                .and_then(|t| t.get("hls_dir"))
                .and_then(|v| v.as_str())
                .unwrap_or("./hls");
            manager.register(ccc_plugin_video_podcast::VideoPodcastPlugin::new(hls_dir))?;
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
            manager.register(ccc_plugin_activitypub::CccPluginActivitypubPlugin)?;
        }
        "ads" => {
            manager.register(ccc_plugin_ads::CccPluginAdsPlugin)?;
        }
        "analytics" => {
            manager.register(ccc_plugin_analytics::CccPluginAnalyticsPlugin)?;
        }
        "newsletter" => {
            manager.register(ccc_plugin_newsletter::CccPluginNewsletterPlugin)?;
        }
        "patreon" => {
            manager.register(ccc_plugin_patreon::CccPluginPatreonPlugin)?;
        }
        "s3" => {
            manager.register(ccc_plugin_s3::CccPluginS3Plugin)?;
        }
        "seo" => {
            manager.register(ccc_plugin_seo::CccPluginSeoPlugin)?;
        }
        "theme-dark-matter" => {
            manager.register(ccc_plugin_theme_dark_matter::CccPluginThemeDarkMatterPlugin)?;
        }
        "whisper" => {
            manager.register(ccc_plugin_whisper::CccPluginWhisperPlugin)?;
        }
        other => return Err(DispatchError::Unknown(other.to_string())),
    }
    Ok(())
}

/// Every identifier `register_by_name` understands. Keep in sync with the match arms above.
pub fn known_plugins() -> &'static [&'static str] {
    &[
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
