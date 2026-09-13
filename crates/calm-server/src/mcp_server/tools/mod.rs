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
pub mod task_dispatch;
pub mod task_repair;
pub mod terminal;
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
    terminal::register_into(registry);
    emit::register_into(registry);
    plan::register_into(registry);
    task_dispatch::register_into(registry);
    task_repair::register_into(registry);
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

#[cfg(test)]
mod tests {
    use crate::mcp_server::build_default_registry;
    use crate::model::CardRole;
    use serde::Serialize;
    use serde_json::Value;
    use std::path::Path;

    /// Whole-surface pin of the default MCP tool registry (#1635 S1d): every
    /// registered descriptor — hidden drill-ins and deprecated aliases
    /// included — serialised field by field, sorted by name. Any change to a
    /// name, description, schema, annotation block or visibility list shows
    /// up as a reviewable diff here; regenerate with
    /// `REGEN_MCP_TOOL_REGISTRY_GOLDEN=1`, then hand-verify the diff.
    const MCP_TOOL_REGISTRY_GOLDEN: &str =
        include_str!("../../../tests/goldens/mcp_tool_registry.json");

    #[derive(Serialize)]
    struct GoldenRow<'a> {
        name: &'a str,
        description: &'a str,
        input_schema: &'a Value,
        annotations: &'a Option<Value>,
        /// Serde strings of `CardRole` (`"planner"`, `"worker"`, …), not the
        /// Rust variant names.
        visible_to_roles: &'a [CardRole],
    }

    fn render_registry_golden() -> String {
        let mut descriptors = build_default_registry().descriptors();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        let rows: Vec<GoldenRow<'_>> = descriptors
            .iter()
            .map(|descriptor| GoldenRow {
                name: &descriptor.name,
                description: &descriptor.description,
                input_schema: &descriptor.input_schema,
                annotations: &descriptor.annotations,
                visible_to_roles: descriptor.visible_to_roles,
            })
            .collect();
        let mut rendered =
            serde_json::to_string_pretty(&rows).expect("serialize registry golden rows");
        rendered.push('\n');
        rendered
    }

    #[test]
    fn default_registry_matches_full_golden() {
        let rendered = render_registry_golden();

        if std::env::var_os("REGEN_MCP_TOOL_REGISTRY_GOLDEN").is_some() {
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/mcp_tool_registry.json");
            std::fs::write(&path, &rendered).expect("write regenerated golden");
            panic!(
                "mcp_tool_registry.json regenerated from the current registry; \
                 hand-verify the diff, commit, and re-run without REGEN_MCP_TOOL_REGISTRY_GOLDEN"
            );
        }

        assert!(
            !MCP_TOOL_REGISTRY_GOLDEN.is_empty(),
            "registry golden degenerate state: the committed golden must not be empty"
        );
        if MCP_TOOL_REGISTRY_GOLDEN == rendered {
            return;
        }
        let (line_number, expected_line, actual_line) = MCP_TOOL_REGISTRY_GOLDEN
            .lines()
            .zip(rendered.lines())
            .enumerate()
            .find(|(_, (expected, actual))| expected != actual)
            .map(|(index, (expected, actual))| (index + 1, expected, actual))
            .unwrap_or_else(|| {
                let shorter = MCP_TOOL_REGISTRY_GOLDEN
                    .lines()
                    .count()
                    .min(rendered.lines().count());
                (shorter + 1, "<end of golden>", "<end of rendered>")
            });
        panic!(
            "registry golden mismatch at line {line_number} (golden {} bytes, rendered {} bytes)\n\
             expected: {expected_line}\n  actual: {actual_line}\n\
             (REGEN_MCP_TOOL_REGISTRY_GOLDEN=1 rewrites the golden; hand-verify the diff)",
            MCP_TOOL_REGISTRY_GOLDEN.len(),
            rendered.len()
        );
    }
}
