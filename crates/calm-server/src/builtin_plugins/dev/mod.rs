//! Development capability: Git payload construction and candidate-bound publication.
pub mod git_actions;
pub mod publish;

pub(crate) use plugin::builtin::gitforge::PLUGIN_ID;

pub(super) fn component(definition: &'static plugin::builtin::Definition) -> super::BuiltinPlugin {
    let mut native = crate::mcp_server::registry::ToolRegistry::new();
    publish::register_into(&mut native);
    super::BuiltinPlugin::new(
        definition,
        native,
        git_actions::lower,
        git_actions::lower_for_caller,
        plugin::builtin::gitforge::INSTRUCTIONS,
    )
}
