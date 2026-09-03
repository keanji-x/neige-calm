//! Per-tool handlers for the kernel-as-MCP-server. PR7a (#136),
//! extended in PR7b with track-state tools.
//!
//! Each submodule defines one or more
//! [`crate::mcp_server::registry::ToolHandler`]s plus their tools/list
//! descriptors. The single public entry point is
//! [`register_default_tools`], which the boot path calls once to
//! populate the [`ToolRegistry`].

use crate::mcp_server::registry::ToolRegistry;

pub mod admin;
pub mod emit;
pub(crate) mod lifecycle_args;
pub mod plan;
pub mod report_links;
pub mod review;
pub mod track_file;
pub mod track_history;
pub mod track_rename;
pub mod track_report;
pub mod track_report_blocks;
pub mod track_state;

/// Register every default tool onto a fresh registry. Callers build
/// the final `Arc<ToolRegistry>` from the result.
///
/// The default set covers emit tools, track-state reads, verdict writes,
/// track-report tools, and read-only track-file views.
///
/// #293 cutover: the old `calm.wait_for_events` long-poll tool is gone —
/// planner agents are driven by pushed turn inputs, not polling.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    emit::register_into(registry);
    plan::register_into(registry);
    report_links::register_into(registry);
    review::register_into(registry);
    track_rename::register_into(registry);
    track_state::register_into(registry);
    track_report::register_into(registry);
    track_report_blocks::register_into(registry);
    track_file::register_into(registry);
    track_history::register_into(registry);
    admin::register_into(registry);
}
