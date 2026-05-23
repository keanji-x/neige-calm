//! Per-tool handlers for the kernel-as-MCP-server. PR7a (#136).
//!
//! Each submodule defines exactly one [`crate::mcp_server::registry::ToolHandler`]
//! plus its tools/list descriptor. The single public entry point is
//! [`register_default_tools`], which the boot path calls once to populate
//! the [`ToolRegistry`] PR7b/PR8 will extend.

use crate::mcp_server::registry::ToolRegistry;

pub mod emit;

/// Register every PR7a tool onto a fresh registry. Callers build the
/// final `Arc<ToolRegistry>` from the result.
///
/// PR7b will extend this with `calm.update_wave_state` /
/// `calm.get_wave_state`; PR8 with `calm.wait_for_events`. Each
/// extension lands as a `register_into` call on the existing
/// registry instance so the boot path stays one line.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    emit::register_into(registry);
}
