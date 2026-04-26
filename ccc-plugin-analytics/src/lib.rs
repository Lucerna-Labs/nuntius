use ccc_core::{Plugin, PluginError};
use std::any::Any;

#[derive(Default)]
pub struct CccPluginAnalyticsPlugin;

impl CccPluginAnalyticsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginAnalyticsPlugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin Analytics"
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
