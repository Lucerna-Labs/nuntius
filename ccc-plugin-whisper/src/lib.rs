use ccc_core::{Plugin, PluginError};
use std::any::Any;

#[derive(Default)]
pub struct CccPluginWhisperPlugin;

impl CccPluginWhisperPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginWhisperPlugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin Whisper"
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
