use ccc_core::{Plugin, PluginError};
use std::any::Any;

pub struct CccPluginActivitypubPlugin;

impl CccPluginActivitypubPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginActivitypubPlugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin Activitypub"
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
