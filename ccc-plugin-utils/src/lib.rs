//! Shared helpers for Nuntius (CCC) plugins.
//!
//! Crates that historically copy-pasted the same 10-line helpers across
//! 10+ plugins should depend on this crate instead. The set is kept small
//! and focused: pure functions plus one generic `TomlSidecar<T>` settings
//! store. Anything that pulls in plugin-specific state stays in the plugin.

#![deny(missing_docs)]

pub mod bytes;
pub mod filename;
pub mod format;
pub mod html;
pub mod render;
pub mod settings;

pub use bytes::human_bytes;
pub use filename::sanitize_filename;
pub use format::{format_seconds, format_unix, is_leap, relative_time, unix_now, unix_to_ymdhm};
pub use html::html_escape;
pub use render::render;
pub use settings::TomlSidecar;
