use ccc_core::{Plugin, PluginError};
use std::any::Any;

pub struct CccPluginS3Plugin;

impl CccPluginS3Plugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CccPluginS3Plugin {
    fn name(&self) -> &'static str {
        "Ccc Plugin S3"
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
