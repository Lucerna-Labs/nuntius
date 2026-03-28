use ccc_core::{Plugin, PluginError};
use std::any::Any;

pub struct CccPluginAdsPlugin;

impl CccPluginAdsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginAdsPlugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin Ads"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing {}...", self.name());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
