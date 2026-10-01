//! Development capability: Git payload construction and candidate-bound publication/review.
pub mod git_actions;
pub mod publish;
pub mod review;

pub(crate) const PLUGIN_ID: &str = "dev.neige.git-forge";

pub(super) fn component() -> super::BuiltinPlugin {
    let mut native = crate::mcp_server::registry::ToolRegistry::new();
    publish::register_into(&mut native);
    review::register_into(&mut native);
    super::BuiltinPlugin::new(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/git-forge/manifest.json"
        )),
        native,
        git_actions::lower,
        git_actions::lower_for_caller,
        include_str!("instructions.md"),
    )
}
