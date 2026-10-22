//! Shared JSON-RPC framing for the kernel-as-MCP-server transport, re-exported from
//! the plugin host, plus the kernel-specific `CalmError` mapping.

pub(crate) use crate::plugin_host::mcp::{
    Frame, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};

/// The one `CalmError` → §5 code mapping for a kernel tool (`docs/conventions/agent-commands.md`);
/// the registry leads the message with the tool name.
pub(crate) fn calm_error(error: crate::error::CalmError) -> RpcError {
    use crate::error::CalmError;
    match error {
        CalmError::BadRequest(message) => RpcError::invalid_params(message),
        CalmError::Forbidden(message) => RpcError::forbidden(format!("forbidden: {message}")),
        CalmError::NotFound(message) => RpcError::not_found(message),
        CalmError::Conflict(message) => RpcError::conflict(message),
        other => RpcError::internal(other.to_string()),
    }
}
