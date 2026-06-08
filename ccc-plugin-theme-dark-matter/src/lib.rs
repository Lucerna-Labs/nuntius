use ccc_core::{Plugin, PluginError};
use std::any::Any;

#[derive(Default)]
pub struct CccPluginThemeDarkMatterPlugin;

impl CccPluginThemeDarkMatterPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginThemeDarkMatterPlugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin Theme Dark Matter"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&self) -> Result<(), PluginError> {
        println!("Initializing {}...", self.name());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
