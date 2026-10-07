//! The shared action contract; kernel submission/execution remain in the transport adapter.
pub(crate) use calm_types::forge_action::{PluginForgePayload, forge_action_payload};
pub(crate) fn semantic_payload_hash(
    payload: &PluginForgePayload,
) -> Result<String, crate::plugin_host::RpcError> {
    calm_types::forge_action::semantic_payload_hash(payload)
        .map_err(crate::plugin_host::RpcError::internal)
}
