//! Gitforge declarations and domain behavior.
pub mod git_actions;
pub mod publish_scripts;
pub const PLUGIN_ID: &str = "gitforge";
pub const INSTRUCTIONS: &str = include_str!("instructions.md");
pub const MANIFEST: &str = include_str!("../../../../../plugins/git-forge/manifest.json");
pub mod publish;
