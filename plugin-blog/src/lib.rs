use azuracast_core::{Plugin, PluginError};
use std::any::Any;

/// A simple Static Blog plugin for AzuraCast.
pub struct StaticBlogPlugin {
    posts: Vec<String>,
}

impl StaticBlogPlugin {
    /// Create a new instance of the Static Blog plugin.
    pub fn new() -> Self {
        Self { posts: Vec::new() }
    }

    /// Add a post to the blog.
    pub fn add_post(&mut self, content: String) {
        self.posts.push(content);
    }

    /// Get all blog posts.
    pub fn get_posts(&self) -> &[String] {
        &self.posts
    }
}

impl Plugin for StaticBlogPlugin {
    fn name(&self) -> &'static str {
        "Static Blog"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing Static Blog Plugin...");
        // Here we could register routes, db tables, or template hooks
        self.add_post("Welcome to the AzuraCast Rust rewrite!".to_string());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
