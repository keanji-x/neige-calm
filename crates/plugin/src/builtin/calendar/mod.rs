//! Calendar declarations and domain service.
pub mod model;
pub const PLUGIN_ID: &str = "calendar";
pub const INSTRUCTIONS: &str = include_str!("instructions.md");
pub const MANIFEST: &str = include_str!("manifest.json");
pub mod ports;
pub mod store;
pub mod tools;
pub mod wake;
