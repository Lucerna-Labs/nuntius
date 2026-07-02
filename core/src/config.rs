//! Configuration schema for the Nuntius (CCC).
//!
//! `ccc.toml` is the single source of truth for which plugins are *enabled*.
//! Disabled plugins are never instantiated — see the `Plugin` trait docs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration loaded from `ccc.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CccConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub site: SiteConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Per-plugin configuration sections. Each plugin reads its own subtable.
    #[serde(default)]
    pub plugin: toml::value::Table,
}

/// Deployment identity — what THIS site calls itself on public pages
/// (header brand, page titles, RSS channel names). Operators set their
/// own name here; "Nuntius" is only the out-of-the-box fallback and
/// remains the platform's name in admin chrome and "Powered by" lines
/// regardless of this setting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteConfig {
    #[serde(default = "default_site_name")]
    pub name: String,
}

fn default_site_name() -> String {
    "Nuntius".into()
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            name: default_site_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 3000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("ccc.redb"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    /// Plugin identifiers (kebab-case names) to instantiate at startup.
    /// Anything not in this list is *not constructed*.
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config IO error: {}", e),
            ConfigError::Parse(e) => write!(f, "config parse error: {}", e),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

impl CccConfig {
    /// Load and parse `ccc.toml` from a path. Missing file → default config.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg: CccConfig = toml::from_str(&raw)?;
        Ok(cfg)
    }
}

impl Default for CccConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            site: SiteConfig::default(),
            plugins: PluginsConfig::default(),
            plugin: toml::value::Table::new(),
        }
    }
}
