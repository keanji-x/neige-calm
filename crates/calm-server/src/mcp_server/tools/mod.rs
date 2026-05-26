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
/// * PR7a registered the three emit tools (`calm.dispatch_request`,
///   `calm.task_completed`, `calm.task_failed`).
/// * PR7b adds the three wave-state tools
///   (`calm.get_wave_state`, `calm.update_wave_state`,
///   `calm.update_task_meta`).
/// * Issue #229 PR B adds the three wave-report tools
///   (`calm.report.read`, `calm.report.write`, `calm.report.edit`) —
///   spec-only, mirror codex's native Read/Edit/Write file tools 1:1
///   so the agent maintains the wave report as if it were a file.
///
/// #293 cutover: the old `calm.wait_for_events` long-poll tool is gone —
/// spec agents are driven by pushed turn inputs, not polling.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    emit::register_into(registry);
    wave_state::register_into(registry);
    wave_report::register_into(registry);
    wave_file::register_into(registry);
}
