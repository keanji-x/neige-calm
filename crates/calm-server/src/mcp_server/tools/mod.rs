//! Per-tool handlers for the kernel-as-MCP-server; [`register_default_tools`] is the single
//! entry point the boot path calls to populate the [`ToolRegistry`].

use crate::mcp_server::registry::ToolRegistry;

pub mod admin;
pub mod area_reports;
pub mod emit;
pub mod mail;
pub(crate) mod paging;
pub mod plan;
pub mod preview;
pub mod report_links;
pub mod report_tag;
pub mod source;
pub mod terminal;
pub mod track_add;
pub mod track_file;
pub mod track_history;
pub mod track_rename;
pub mod track_report;
pub mod track_report_blocks;
pub(crate) mod track_report_hydrate;
pub mod track_state;
pub mod user_ask;
pub mod workspace_reports;
pub(crate) mod write_args;

/// Register every default tool onto a fresh registry.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    terminal::register_into(registry);
    emit::register_into(registry);
    plan::register_into(registry);
    report_links::register_into(registry);
    report_tag::register_into(registry);
    area_reports::register_into(registry);
    source::register_into(registry);
    track_rename::register_into(registry);
    user_ask::register_into(registry);
    mail::register_into(registry);
    preview::register_into(registry);
    track_state::register_into(registry);
    track_add::register_into(registry);
    track_report::register_into(registry);
    track_report_blocks::register_into(registry);
    track_file::register_into(registry);
    track_history::register_into(registry);
    crate::builtin_plugins::register_native_tools(registry);
    admin::register_into(registry);
    workspace_reports::register_into(registry);
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

    /// Whole-surface pin of the default MCP tool registry, sorted by name, with the description
    /// as `description_sha256` (the wording lives in `prompts/tools/<tool>.md`).
    /// Regenerate with `REGEN_MCP_TOOL_REGISTRY_GOLDEN=1`, then hand-verify the diff.
    const MCP_TOOL_REGISTRY_GOLDEN: &str =
        include_str!("../../../tests/goldens/mcp_tool_registry.json");

    /// Anti-vacuity floor: an empty registry rendered against an empty `[]` golden must not pass.
    const MIN_GOLDEN_ROWS: usize = 30;

    #[derive(Serialize)]
    struct GoldenRow<'a> {
        name: &'a str,
        /// Lowercase hex SHA-256 of the description's UTF-8 bytes.
        description_sha256: String,
        input_schema: &'a Value,
        /// Presence-encoded like the wire: `None` has no `annotations` key, `Some(Value::Null)`
        /// renders `"annotations": null`.
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<&'a Value>,
        /// Serde strings of `CardRole`, not the Rust variant names: who may call the tool.
        roles: &'a [CardRole],
        /// Who sees it in `tools/list`, a subset of `roles`.
        listed_for: &'a [CardRole],
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
            roles: descriptor.roles,
            listed_for: descriptor.listed_for,
        }
    }

    /// Pretty JSON array of one row per descriptor, plus one trailing newline.
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

    /// What Codex loads up front for a Planner (#1893): every visible tool's description plus its
    /// compact input schema. One-sided caps; a change that shrinks the surface lowers them.
    #[test]
    fn planner_tool_surface_fits_its_byte_budget() {
        // #2130: measured 29,796 bytes across 32 Planner tools before `neige_mail_send` (798
        // bytes: description 536, schema 262); trimming restated schema facts from eight existing
        // descriptions freed 164, and the cap rose by the remaining 430 to the measured 30,430
        // across 33 tools. #2104 K1 added `neige_track_add` without raising it: trims to the
        // largest descriptions paid for it. Keep the aggregate bound and the per-description cap.
        // #2209: `neige_user_ask` (995 bytes: description 697, schema 298) replaced the ratify
        // and notify tools (1,285) and the mail hand-off line lost 3, so the measured 30,413 fell
        // to 30,120 and the cap follows it down.
        const SURFACE_MAX_BYTES: usize = 30_120;
        const DESCRIPTION_MAX_BYTES: usize = 2_048;

        let descriptors = build_default_registry().descriptors_listed_for(CardRole::Planner);
        assert!(
            descriptors.len() >= 20,
            "anti-vacuity: the Planner sees {} tools",
            descriptors.len()
        );
        let mut total = 0;
        for descriptor in &descriptors {
            let description = descriptor.description.len();
            assert!(
                description <= DESCRIPTION_MAX_BYTES,
                "{}: description is {description} bytes, over {DESCRIPTION_MAX_BYTES}",
                descriptor.name
            );
            let schema = serde_json::to_string(&descriptor.input_schema)
                .expect("serialize input schema")
                .len();
            total += description + schema;
        }
        assert!(
            total <= SURFACE_MAX_BYTES,
            "the Planner tool surface is {total} bytes, over its {SURFACE_MAX_BYTES} byte budget"
        );
    }

    /// A plain `Option<Value>` serialisation would render `None` and `Some(Value::Null)` both as `null`.
    #[test]
    fn golden_row_encodes_annotations_presence_like_the_wire() {
        let descriptor = |annotations: Option<Value>| ToolDescriptor {
            name: "neige_fixture_tool".to_string(),
            description: "fixture".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            annotations,
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
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

    /// Every tool's description renders `prompts/tools/<tool name>.md` with shared acceptance
    /// guidance, and there is no such file without a tool. Trailing whitespace before the final
    /// newline would make the embedded description differ from the file's visible content.
    #[test]
    fn prompt_files_cover_exactly_the_registered_tools() {
        let registry = build_default_registry();

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/tools");
        let mut stems = BTreeSet::new();
        let mut contents = std::collections::BTreeMap::new();
        let directories = std::iter::once(dir).chain(
            plugin::builtin::catalog()
                .iter()
                .map(|definition| Path::new(definition.tool_prompt_directory).to_path_buf()),
        );
        for entry in directories.flat_map(|directory| {
            std::fs::read_dir(directory).expect("read declared tool prompt directory")
        }) {
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
            assert!(
                stems.insert(stem.to_string()),
                "duplicate tool prompt: {stem}"
            );
            contents.insert(stem.to_string(), body.to_string());
        }

        let mut expected = BTreeSet::new();
        for descriptor in registry.descriptors() {
            let file_body = contents.get(&descriptor.name).unwrap_or_else(|| {
                panic!(
                    "{}: no prompts/tools/{}.md",
                    descriptor.name, descriptor.name
                )
            });
            assert_eq!(
                descriptor.description,
                calm_types::observation::render_task_acceptance_guidance(file_body),
                "{}: description does not render prompts/tools/{}.md",
                descriptor.name,
                descriptor.name
            );
            expected.insert(descriptor.name);
        }
        assert_eq!(
            stems, expected,
            "prompts/tools/*.md stems must be exactly the registered tool names"
        );
        assert!(
            expected.len() >= 30,
            "anti-vacuity floor: {} tools",
            expected.len()
        );
    }

    fn kernel_tool_names() -> Vec<String> {
        let kernel: Vec<String> = build_default_registry()
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .filter(|name| !name.starts_with(crate::plugin_results::PLUGIN_TOOL_PREFIX))
            .collect();
        assert!(kernel.len() >= 30, "anti-vacuity: {kernel:?}");
        kernel
    }

    /// The built-in plugins' compiled tools the kernel registry also holds, each with its
    /// plugin's id: plugin tools (`plugin_<id>_<tool>`, #2227), so the kernel-only checks skip
    /// them and the checks below name them.
    fn compiled_plugin_tools() -> Vec<(String, String)> {
        let compiled: Vec<(String, String)> = build_default_registry()
            .descriptors()
            .into_iter()
            .filter_map(|descriptor| {
                crate::builtin_plugins::owner(&descriptor.name)
                    .map(|plugin| (plugin.manifest().id.clone(), descriptor.name))
            })
            .collect();
        assert!(compiled.len() >= 5, "anti-vacuity: {compiled:?}");
        compiled
    }

    /// #2087 §2: every kernel tool is `neige_<object>_<action>`, each segment one word, so the
    /// CLI command is the name split at `_`. No kernel object is `plugin`, the word that starts
    /// every minted plugin tool name (`plugin_<id>_<tool>`).
    #[test]
    fn kernel_tool_names_follow_the_grammar() {
        let grammar = regex::Regex::new(r"^neige_([a-z0-9]+)_[a-z0-9]+$").expect("grammar regex");
        let off_grammar: Vec<String> = kernel_tool_names()
            .into_iter()
            .filter(|name| {
                grammar
                    .captures(name)
                    .is_none_or(|words| &words[1] == "plugin")
            })
            .collect();
        assert!(
            off_grammar.is_empty(),
            "kernel tools outside `neige_<object>_<action>`: {off_grammar:?}"
        );
    }

    /// The plugin names `tools/list` serves: every built-in (manifest and compiled tools), every
    /// repository manifest under `plugins/` and a connector whose id and upstream tool names carry
    /// `.` and `-`, all running.
    fn served_plugin_tool_names() -> Vec<String> {
        use crate::mcp_server::tool_visibility::{ToolDiscoveryScope, TrackPluginScope};
        use crate::plugin_host::Manifest;
        use serde_json::json;

        let mut manifests: Vec<Manifest> = crate::builtin_plugins::catalog()
            .iter()
            .map(|builtin| builtin.manifest().clone())
            .collect();
        let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        for entry in std::fs::read_dir(&plugins).expect("read plugins/") {
            let path = entry.expect("plugins/ entry").path().join("manifest.json");
            if let Ok(text) = std::fs::read_to_string(&path) {
                let manifest = Manifest::parse(&text)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                if !manifests.iter().any(|m| m.id == manifest.id) {
                    manifests.push(manifest);
                }
            }
        }
        let mut connector = Manifest::parse(
            &json!({
                "manifest_version": 1, "kind": "mcp-http", "id": "mcp-wis.burg",
                "version": "0.1.0", "min_kernel_version": "0.0.1", "display_name": "Fixture",
                "mcp_http": { "url": "https://mcp.example.com/mcp", "tools_all": true },
            })
            .to_string(),
        )
        .expect("connector manifest");
        let block = connector.mcp_http.clone().expect("mcp_http");
        connector.exposes_tools = crate::plugin_host::connector::materialize_http_tools(
            &connector.id,
            &block,
            &[
                json!({ "name": "foo.bar" }),
                json!({ "name": "get-report-detail" }),
            ],
        );
        manifests.push(connector);
        let running: std::collections::BTreeSet<String> =
            manifests.iter().map(|m| m.id.clone()).collect();
        // Each plugin as its own bound Track sees it, so `bound-track` tools are served too.
        let names: Vec<String> = manifests
            .iter()
            .flat_map(|manifest| {
                crate::mcp_server::transport::plugin_tool_descriptors_from(
                    manifests.clone(),
                    &running,
                    &ToolDiscoveryScope::Track(&TrackPluginScope::Only(manifest.id.clone())),
                )
            })
            .map(|descriptor| descriptor.name)
            .chain(compiled_plugin_tools().into_iter().map(|(_, name)| name))
            .collect();
        assert!(
            names.contains(&"plugin_mcp_wis_burg_foo_bar".to_string())
                && names.contains(&"plugin_gitforge_gh_pr_checks".to_string())
                && names.contains(&"plugin_gitforge_publish".to_string())
                && names.len() >= 20,
            "anti-vacuity: {names:?}"
        );
        names
    }

    /// #2087 §2/§6: every served name, kernel and plugin tools alike, is in the provider alphabet
    /// `[A-Za-z0-9_]`, so no client respells it and the model sees exactly the name prompts and
    /// help print.
    #[test]
    fn served_tool_names_use_the_word_alphabet() {
        let outside: Vec<String> = kernel_tool_names()
            .into_iter()
            .chain(served_plugin_tool_names())
            .filter(|name| {
                name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .collect();
        assert!(
            outside.is_empty(),
            "served tool names outside [A-Za-z0-9_]: {outside:?}"
        );
    }

    /// #2087 §4/§8: every input key of every kernel tool and every compiled plugin tool (#2227),
    /// recursively through `properties`, `items` and `oneOf`/`anyOf`/`allOf`, is snake_case (an
    /// opaque `payload` is skipped), and no top-level key is a retired name. `until` stays legal
    /// nested (a recurrence's last day), and so does `report_commit`'s `ops[].id` (a block id, §9).
    #[test]
    fn kernel_tool_params_use_the_vocabulary() {
        const RETIRED_TOP_LEVEL: &[&str] = &[
            "id",
            "after",
            "cancelled",
            "time_zone",
            "request_id",
            "select",
            "until",
        ];
        fn snake(key: &str) -> bool {
            key.split('_').all(|word| {
                word.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    && word
                        .bytes()
                        .next()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric())
            }) && key.starts_with(|c: char| c.is_ascii_lowercase())
        }
        fn walk(schema: &Value, path: &str, top: bool, out: &mut Vec<String>, keys: &mut usize) {
            let Some(object) = schema.as_object() else {
                return;
            };
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (key, child) in properties {
                    *keys += 1;
                    if !snake(key) {
                        out.push(format!("{path}.{key}: not snake_case"));
                    }
                    if top && RETIRED_TOP_LEVEL.contains(&key.as_str()) {
                        out.push(format!("{path}.{key}: retired top-level name"));
                    }
                    if key != "payload" {
                        walk(child, &format!("{path}.{key}"), false, out, keys);
                    }
                }
            }
            if let Some(items) = object.get("items") {
                walk(items, &format!("{path}[]"), false, out, keys);
            }
            for keyword in ["oneOf", "anyOf", "allOf"] {
                for (index, branch) in object
                    .get(keyword)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    walk(branch, &format!("{path}|{keyword}{index}"), top, out, keys);
                }
            }
        }
        let registry = build_default_registry();
        let names: Vec<String> = kernel_tool_names()
            .into_iter()
            .chain(compiled_plugin_tools().into_iter().map(|(_, name)| name))
            .collect();
        let (mut off, mut keys) = (Vec::new(), 0);
        for descriptor in registry
            .descriptors()
            .into_iter()
            .filter(|descriptor| names.contains(&descriptor.name))
        {
            walk(
                &descriptor.input_schema,
                &descriptor.name,
                true,
                &mut off,
                &mut keys,
            );
        }
        assert!(keys >= 100, "anti-vacuity: only {keys} input keys walked");
        assert!(
            off.is_empty(),
            "kernel tool inputs outside the §4 vocabulary: {off:#?}"
        );
    }

    /// #2087 §3/§8: a kernel tool's action is one verb of the closed vocabulary, written out here
    /// so a new verb needs `docs/conventions/agent-commands.md` §3 changed first, and its object is
    /// one word.
    #[test]
    fn kernel_tool_actions_are_in_the_vocabulary() {
        const VERBS: &[&str] = &[
            "ls", "cat", "show", "status", "log", "diff", "find", "describe", "read", "write",
            "commit", "tag", "rename", "add", "set", "rm", "capture", "ask", "send", "input",
            "control", "open", "close", "cancel", "publish", "accept", "reject", "done", "fail",
            "gc", "vacuum",
        ];
        let word = regex::Regex::new(r"^[a-z0-9]+$").expect("word regex");
        let outside: Vec<String> = kernel_tool_names()
            .into_iter()
            .filter(|name| {
                let segments: Vec<&str> = name.split('_').collect();
                !matches!(segments.as_slice(), ["neige", object, action]
                    if word.is_match(object) && VERBS.contains(action))
            })
            .collect();
        assert!(
            outside.is_empty(),
            "kernel tools whose action is not a §3 verb or whose object is not one word: \
             {outside:?}"
        );
        // §6: a compiled plugin tool's own name is `[<object>_]<verb>` in the same words, under
        // its plugin's minted prefix.
        let outside: Vec<String> = compiled_plugin_tools()
            .into_iter()
            .filter(|(id, name)| {
                let local = name.strip_prefix(&crate::plugin_results::registry_name(id, ""));
                let segments: Vec<&str> = local.map_or_else(Vec::new, |l| l.split('_').collect());
                !matches!(segments.as_slice(), [.., action]
                    if segments.iter().all(|s| word.is_match(s)) && VERBS.contains(action))
            })
            .map(|(_, name)| name)
            .collect();
        assert!(
            outside.is_empty(),
            "compiled plugin tools outside `plugin_<id>_[<object>_]<verb>`: {outside:?}"
        );
    }
}
