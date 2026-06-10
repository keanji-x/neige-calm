//! Per-tool handlers for the kernel-as-MCP-server. PR7a (#136),
//! extended in PR7b with wave-state tools.
//!
//! Each submodule defines one or more
//! [`crate::mcp_server::registry::ToolHandler`]s plus their tools/list
//! descriptors. The single public entry point is
//! [`register_default_tools`], which the boot path calls once to
//! populate the [`ToolRegistry`].

use crate::mcp_server::registry::ToolRegistry;

pub mod emit;
pub mod wave_file;
pub mod wave_report;
pub mod wave_state;

/// Register every default tool onto a fresh registry. Callers build
/// the final `Arc<ToolRegistry>` from the result.
///
/// The default set covers emit tools, wave-state tools, wave-report
/// tools, and read-only wave-file views.
///
/// #293 cutover: the old `calm.wait_for_events` long-poll tool is gone —
/// spec agents are driven by pushed turn inputs, not polling.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    emit::register_into(registry);
    wave_state::register_into(registry);
    wave_report::register_into(registry);
    wave_file::register_into(registry);
}
