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
    use crate::mcp_server::registry::ToolDescriptor;
    use crate::model::CardRole;
    use serde::Serialize;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Whole-surface pin of the default MCP tool registry (#1635 S1d): every
    /// registered descriptor — hidden drill-ins and deprecated aliases
    /// included — sorted by name, with the protocol fields (`name`,
    /// `input_schema`, `annotations`, `visible_to_roles`) verbatim and the
    /// description as `description_sha256`. The wording itself lives in
    /// `prompts/tools/<tool>.md`, where a change is reviewable as a diff of
    /// that file; the hash here proves the registry serves the file bytes
    /// minus the single final newline (the structural test
    /// `prompt_files_cover_exactly_the_non_alias_tools` pins that equality)
    /// and, for the aliases, exactly the `format!`ed protocol string —
    /// without copying 90 KB of prose into a second file.
    /// Regenerate with `REGEN_MCP_TOOL_REGISTRY_GOLDEN=1`, then hand-verify
    /// the diff.
    const MCP_TOOL_REGISTRY_GOLDEN: &str =
        include_str!("../../../tests/goldens/mcp_tool_registry.json");

    /// Anti-vacuity floor for the golden: an empty registry rendered against
    /// an empty `[]` golden must not pass.
    const MIN_GOLDEN_ROWS: usize = 30;

    #[derive(Serialize)]
    struct GoldenRow<'a> {
        name: &'a str,
        /// Lowercase hex SHA-256 of the description's UTF-8 bytes.
        description_sha256: String,
        input_schema: &'a Value,
        /// Presence-encoded like the wire (`transport.rs` omits the
        /// `annotations` key for `None`): a descriptor with `None` has no
        /// `annotations` key in its row, `Some(Value::Null)` renders
        /// `"annotations": null`.
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<&'a Value>,
        /// Serde strings of `CardRole` (`"planner"`, `"worker"`, …), not the
        /// Rust variant names.
        visible_to_roles: &'a [CardRole],
    }

    fn description_sha256(description: &str) -> String {
        let digest = Sha256::digest(description.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn golden_row(descriptor: &ToolDescriptor) -> GoldenRow<'_> {
        GoldenRow {
            name: &descriptor.name,
            description_sha256: description_sha256(&descriptor.description),
            input_schema: &descriptor.input_schema,
            annotations: descriptor.annotations.as_ref(),
            visible_to_roles: descriptor.visible_to_roles,
        }
    }

    /// Pretty JSON array of one row per descriptor, in the given order, plus
    /// one trailing newline.
    fn render_golden_rows(descriptors: &[ToolDescriptor]) -> String {
        let rows: Vec<GoldenRow<'_>> = descriptors.iter().map(golden_row).collect();
        let mut rendered =
            serde_json::to_string_pretty(&rows).expect("serialize registry golden rows");
        rendered.push('\n');
        rendered
    }

    fn render_registry_golden() -> String {
        let mut descriptors = build_default_registry().descriptors();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        assert!(
            descriptors.len() >= MIN_GOLDEN_ROWS,
            "registry golden degenerate state: only {} descriptors registered",
            descriptors.len()
        );
        render_golden_rows(&descriptors)
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
        let golden_rows: Vec<Value> =
            serde_json::from_str(MCP_TOOL_REGISTRY_GOLDEN).expect("parse mcp_tool_registry.json");
        assert!(
            golden_rows.len() >= MIN_GOLDEN_ROWS,
            "registry golden degenerate state: only {} rows in the committed golden",
            golden_rows.len()
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

    /// The golden must tell `annotations: None` (wire: key absent) apart from
    /// `Some(Value::Null)` (wire: `"annotations": null`); a plain
    /// `Option<Value>` serialisation renders both as `null`.
    #[test]
    fn golden_row_encodes_annotations_presence_like_the_wire() {
        let descriptor = |annotations: Option<Value>| ToolDescriptor {
            name: "calm.fixture.tool".to_string(),
            description: "fixture".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            annotations,
            visible_to_roles: &[CardRole::Planner],
        };
        let absent = render_golden_rows(&[descriptor(None)]);
        let null = render_golden_rows(&[descriptor(Some(Value::Null))]);
        assert_ne!(absent, null, "None and Some(Null) must render differently");

        let absent_row: Vec<Value> = serde_json::from_str(&absent).expect("parse absent row");
        let null_row: Vec<Value> = serde_json::from_str(&null).expect("parse null row");
        assert!(
            absent_row[0].get("annotations").is_none(),
            "None must omit the key: {absent}"
        );
        assert_eq!(
            null_row[0].get("annotations"),
            Some(&Value::Null),
            "Some(Null) must render an explicit null: {null}"
        );
        // Presence is the only difference between the two rows.
        let mut null_without_key = null_row[0].clone();
        null_without_key
            .as_object_mut()
            .expect("row is an object")
            .remove("annotations");
        assert_eq!(absent_row[0], null_without_key);
    }

    /// #1635 S1d: every non-alias tool's description is the file
    /// `prompts/tools/<tool name>.md` (embedded with `include_str!` and
    /// `trim_end()`), and there is no such file without a tool. The
    /// directory is read at test time, so a stray, renamed or orphaned file
    /// fails here rather than silently going unused. The byte rules make
    /// `trim_end()` strip exactly the repository newline and nothing else:
    /// with trailing whitespace before that newline, the embedded description
    /// would differ from the file's visible content.
    #[test]
    fn prompt_files_cover_exactly_the_non_alias_tools() {
        let registry = build_default_registry();
        let aliases = registry.deprecated_alias_names();
        assert!(
            !aliases.is_empty(),
            "the default registry is expected to carry deprecated aliases"
        );

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/tools");
        let mut stems = BTreeSet::new();
        let mut contents = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("read prompts/tools") {
            let path = entry.expect("directory entry").path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_else(|| panic!("{}: non-UTF-8 file name", path.display()));
            let stem = file_name.strip_suffix(".md").unwrap_or_else(|| {
                panic!("{}: only `<tool>.md` files belong here", path.display())
            });
            assert!(
                path.is_file(),
                "{}: only plain files belong here",
                path.display()
            );
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(!content.is_empty(), "{file_name}: empty");
            assert!(!content.contains('\r'), "{file_name}: carriage return");
            let body = content
                .strip_suffix('\n')
                .unwrap_or_else(|| panic!("{file_name}: must end with a newline"));
            assert!(
                !body.ends_with('\n'),
                "{file_name}: must end with exactly one newline"
            );
            assert!(
                !body.ends_with(char::is_whitespace),
                "{file_name}: trailing whitespace before the final newline"
            );
            assert!(!body.is_empty(), "{file_name}: newline only");
            stems.insert(stem.to_string());
            contents.insert(stem.to_string(), body.to_string());
        }

        let mut expected = BTreeSet::new();
        for descriptor in registry.descriptors() {
            if aliases.contains(&descriptor.name) {
                continue;
            }
            let file_body = contents.get(&descriptor.name).unwrap_or_else(|| {
                panic!(
                    "{}: no prompts/tools/{}.md",
                    descriptor.name, descriptor.name
                )
            });
            assert_eq!(
                &descriptor.description, file_body,
                "{}: description is not the content of prompts/tools/{}.md",
                descriptor.name, descriptor.name
            );
            expected.insert(descriptor.name);
        }
        assert_eq!(
            stems, expected,
            "prompts/tools/*.md stems must be exactly the non-alias tool names"
        );
        assert!(
            expected.len() >= 30,
            "anti-vacuity floor: {} non-alias tools",
            expected.len()
        );
    }
}
