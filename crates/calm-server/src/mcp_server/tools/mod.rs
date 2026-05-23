//! Per-tool handlers for the kernel-as-MCP-server. PR7a (#136),
//! extended in PR7b with wave-state tools.
//!
//! Each submodule defines one or more
//! [`crate::mcp_server::registry::ToolHandler`]s plus their tools/list
//! descriptors. The single public entry point is
//! [`register_default_tools`], which the boot path calls once to
//! populate the [`ToolRegistry`] PR8 will extend further.

use crate::mcp_server::registry::ToolRegistry;

pub mod emit;
pub mod wait;
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
/// * PR8 adds `calm.wait_for_events` — spec-only long-poll over the
///   wave's event stream + matching `/internal/codex/pending_events`
///   HTTP fallback for the bridge's Stop-hook handler.
/// * Issue #229 PR B adds the three wave-report tools
///   (`calm.report.read`, `calm.report.write`, `calm.report.edit`) —
///   spec-only, mirror codex's native Read/Edit/Write file tools 1:1
///   so the agent maintains the wave report as if it were a file.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    emit::register_into(registry);
    wave_state::register_into(registry);
    wait::register_into(registry);
    wave_report::register_into(registry);
}
