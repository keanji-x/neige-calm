//! #2003 stale-name sweep: no retired kernel tool name and no retired MCP server key survives in a
//! tracked file the product ships or runs, so a prompt, refusal, observation, fixture or fe constant
//! that still says the old name is red here instead of misleading an agent at run time.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The retired kernel tool names: the old `calm.` prefix on any kernel object, plus the removed
/// aliases and shims by their own spelling, assembled so this file does not carry the B0 spelling
/// of the retired publish tool, nor the ratify and notify tools `neige_user_ask` replaced (#2209).
const RETIRED_TOOL_NAME: &str = concat!(
    r"\b(?:calm\.(admin|area|calendar|dispatch_request|get_track_state|plan|preview|ratify|report|review|source|task|task_completed|task_failed|terminal|track|update_task_meta|user)|neige_",
    r"(?:track_publish|ratify_request|user_notify))\b"
);

/// #2087 B0: a kernel tool name never contains `.`, so a dotted `neige.<object>.` on any kernel
/// object (or a family such as `neige.<object>.*`) is retired. The plugin-to-host callbacks
/// `neige.kv.*`, `neige.overlay.*`, `neige.card.*` and `neige.event.subscribe` are JSON-RPC
/// methods, not tools, and use none of these objects.
const RETIRED_DOTTED_KERNEL_NAME: &str = r"(?:^|[^A-Za-z0-9_.\-])neige\.(?:admin|area|calendar|dev|dispatch|plan|preview|ratify|report|review|source|task|terminal|track|user|workspace)\.";

/// #2087 B1a: a view tool's action is its Unix/git verb, so the B0 names with a noun or synonym
/// action are retired, as are the CLI spellings of the old track view and tool listing.
const RETIRED_VIEW_NAME: &str = concat!(
    r"\bneige_(?:plan_(?:list|cancel)|source_list|area_outline|report_(?:backlinks|kinds)|",
    r"track_state|workspace_(?:reports|report|changes|edits))\b|\bneige (?:track state|tool list)\b"
);

/// #2087 B1b: the terminal's anchored read is `read` and its lookup is `show`.
const RETIRED_TERMINAL_NAME: &str = r"\bneige_terminal_(?:observe|resolve)\b";

/// #2087 B1c: Calendar and preview use `add`/`ls`/`set`/`rm`, and a verdict is `accept` or `reject`.
const RETIRED_CRUD_NAME: &str =
    r"\bneige_(?:calendar_(?:create|list|update)|preview_(?:register|unregister)|task_verdict)\b";

/// #2087 B5: the built-in plugin ids are `calendar` and `gitforge`, and git-forge's tool names are
/// `_`-separated. A dotted git-forge tool name followed by `:` is an `idem_key` literal, which
/// keeps its bytes so the dedup wall holds.
const RETIRED_PLUGIN_NAME: &str = concat!(
    r"dev\.neige\.(?:calendar|git-forge)(?:[^A-Za-z0-9\-]|$)|(?:^|[^A-Za-z0-9_.\-])(?:git\.worktree\.add|git\.commit|",
    r"gh\.pr\.(?:list|diff|checks|merge)|gh\.issue\.(?:view|close|comments?))(?:[^:A-Za-z0-9_\-]|$)"
);

/// #2227: a built-in's compiled tool comes and goes with its plugin, so it is the plugin tool
/// `plugin_<id>_<tool>` (`plugin_gitforge_publish`, `plugin_calendar_ls`); its kernel-looking
/// spellings are retired.
const RETIRED_NATIVE_NAME: &str = r"\bneige_(?:dev_publish|calendar_(?:add|ls|set|rm))\b";

/// #2323: the dev merge policy that asks before merging is `ask`, the one way the Planner asks the
/// user since #2209; the value named after the retired ratify mechanism is retired with it.
const RETIRED_MERGE_POLICY: &str = r"\bhold-for-ratify\b";

/// The retired MCP server key as a client spells it, assembled from two literals so this file does
/// not carry the token it hunts.
fn retired_server_key() -> String {
    concat!("mcp__", "calm(?:__|\\b)").to_string()
}

/// A deliberate retired-name input (a rejection test) is exempt per line, never per file.
const REJECTION_INPUT_MARKER: &str = "// retired-name: rejection input";

/// The closed allowlist: released migrations and the #2003 and #2087 migrations (one directory,
/// byte-frozen once released), the migrations' tests, a golden of an event written before #2016,
/// and the design document that records the old names.
fn allowlisted(path: &str) -> bool {
    path.starts_with("crates/calm-truth/migrations/")
        || path == "crates/calm-server/tests/cases/neige_tool_name_migration.rs"
        || path == "crates/calm-server/tests/cases/tool_name_separator_migration.rs"
        || path == "crates/calm-server/tests/cases/tool_verbs_migration.rs"
        || path == "crates/calm-server/tests/cases/terminal_verbs_migration.rs"
        || path == "crates/calm-server/tests/cases/crud_verbs_migration.rs"
        || path == "crates/calm-server/tests/cases/plugin_names_migration.rs"
        || path == "crates/calm-server/tests/cases/native_plugin_tool_names_migration.rs"
        || path == "crates/calm-truth/src/db/sqlite/track_plugin_scope_migration_tests.rs"
        || path == "crates/calm-truth/src/db/sqlite/track_template_rename_migration_tests.rs"
        || path == "crates/calm-truth/src/db/sqlite/track_merge_policy_ask_migration_tests.rs"
        || path == "crates/calm-server/tests/goldens/events/forge_pr_merged.historical_subject.json"
        || path == "docs/architecture/2003-cli-mcp-naming.md"
}

/// The scanned roots, relative to the workspace root.
const SCANNED: &[&str] = &["crates", "fe", "plugins", "docs/using-neige-calm.md"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn tracked_files(root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .args(SCANNED)
        .output()
        .expect("run git ls-files");
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 paths")
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn patterns() -> [regex::Regex; 9] {
    [
        regex::Regex::new(RETIRED_TOOL_NAME).expect("tool-name regex"),
        regex::Regex::new(RETIRED_DOTTED_KERNEL_NAME).expect("dotted-name regex"),
        regex::Regex::new(RETIRED_VIEW_NAME).expect("view-name regex"),
        regex::Regex::new(RETIRED_TERMINAL_NAME).expect("terminal-name regex"),
        regex::Regex::new(RETIRED_CRUD_NAME).expect("crud-name regex"),
        regex::Regex::new(RETIRED_PLUGIN_NAME).expect("plugin-name regex"),
        regex::Regex::new(RETIRED_NATIVE_NAME).expect("native-name regex"),
        regex::Regex::new(RETIRED_MERGE_POLICY).expect("merge-policy regex"),
        regex::Regex::new(&retired_server_key()).expect("server-key regex"),
    ]
}

fn offending(line: &str, patterns: &[regex::Regex]) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(line))
        && !line.trim_end().ends_with(REJECTION_INPUT_MARKER)
}

/// The patterns hit the retired spellings and nothing the internal `calm` names still own.
#[test]
fn the_sweep_patterns_hit_only_retired_names() {
    let patterns = patterns();
    for hit in [
        "neige_track_publish",                       // retired-name: rejection input
        "`calm.track.cat_at`",                       // retired-name: rejection input
        "calm.report.write_markdown",                // retired-name: rejection input
        "calm.task_completed",                       // retired-name: rejection input
        "\"calm.user.notify\"",                      // retired-name: rejection input
        "calm.track: no such path",                  // retired-name: rejection input
        "neige.track.cat",                           // retired-name: rejection input
        "`neige.report.*`",                          // retired-name: rejection input
        "(neige.task.report_success)",               // retired-name: rejection input
        "prompts/tools/neige.dev.publish.md",        // retired-name: rejection input
        "`neige_plan_list`",                         // retired-name: rejection input
        "neige_plan_cancel:",                        // retired-name: rejection input
        "(neige_source_list)",                       // retired-name: rejection input
        "neige_area_outline.",                       // retired-name: rejection input
        "neige_report_backlinks",                    // retired-name: rejection input
        "neige_report_kinds",                        // retired-name: rejection input
        "\"neige_track_state\"",                     // retired-name: rejection input
        "prompts/tools/neige_workspace_reports.md",  // retired-name: rejection input
        "neige_workspace_report,",                   // retired-name: rejection input
        "neige_workspace_changes",                   // retired-name: rejection input
        "neige_workspace_edits",                     // retired-name: rejection input
        "run `neige track state`",                   // retired-name: rejection input
        "neige tool list --all",                     // retired-name: rejection input
        "`neige_terminal_observe`",                  // retired-name: rejection input
        "prompts/tools/neige_terminal_resolve.md",   // retired-name: rejection input
        "`neige_calendar_create`",                   // retired-name: rejection input
        "neige_calendar_list and add",               // retired-name: rejection input
        "neige_calendar_update.",                    // retired-name: rejection input
        "\"neige_preview_register\"",                // retired-name: rejection input
        "prompts/tools/neige_preview_unregister.md", // retired-name: rejection input
        "call neige_task_verdict",                   // retired-name: rejection input
        "dev.neige.calendar",                        // retired-name: rejection input
        "`plugin_dev.neige.git-forge_gh.pr.checks`", // retired-name: rejection input
        "read gh.issue.view and",                    // retired-name: rejection input
        "\"gh.pr.checks\" =>",                       // retired-name: rejection input
        "with git.commit.",                          // retired-name: rejection input
        "call neige_ratify_request with",            // retired-name: rejection input
        "\"neige_user_notify\",",                    // retired-name: rejection input
        "prompts/tools/neige_user_notify.md",        // retired-name: rejection input
        "`neige_dev_publish`",                       // retired-name: rejection input
        "neige_calendar_add,",                       // retired-name: rejection input
        "\"neige_calendar_ls\"",                     // retired-name: rejection input
        "prompts/tools/neige_calendar_set.md",       // retired-name: rejection input
        "then neige_calendar_rm.",                   // retired-name: rejection input
        "\"default\": \"hold-for-ratify\",",         // retired-name: rejection input
        "- `hold-for-ratify` — also the semantics",  // retired-name: rejection input
        concat!("mcp__", "calm__neige_report_read"),
        concat!("allowed: mcp__", "calm Edit"),
    ] {
        assert!(offending(hit, &patterns), "must be red: {hit}");
    }
    for miss in [
        "plugin_gitforge_publish",
        "neige.kv.set",
        "neige.overlay.delete",
        "neige.card.create",
        "neige.event.subscribe",
        "calendar",
        "neige.worker.service",
        "neige.track: path not available",
        "calm.db",
        "calm_server::mcp_server",
        "calm-server",
        "calm-truth/migrations",
        "neige_track_show",
        "neige_task_ls",
        "neige_track_status",
        "neige_workspace_ls",
        "neige_report_read",
        "neige_plan_list2",
        "xneige_plan_list",
        "the track state",
        "neige tool ls --all",
        "neige_terminal_read",
        "neige_terminal_show",
        "neige_terminal_observed",
        "the terminal observation",
        "plugin_calendar_ls",
        "plugin_calendar_rm",
        "neige_calendar_lists",
        "neige_calendar_lsx",
        "my_neige_dev_publish",
        "0137_neige_dev_publish.sql",
        "neige_preview_add",
        "neige_preview_registered",
        "neige_task_accept",
        "neige_task_reject",
        "neige_user_ask",
        "ratify.requested",
        "ask",
        "auto-merge",
        "merge_policy `ask`",
        "my_neige_task_verdict",
        "a task verdict",
        "xneige.track.cat",
        "gitforge",
        "plugin_gitforge_gh_pr_checks",
        "format!(\"gh.pr.checks:{repo}:{pr}\")",
        "\"git.commit:d:{delivery_id}\"",
        "dev-neige-market",
        "mcp__neige__neige_track_show",
        "calm.css",
        "calm.theme",
        "xcalm.track.cat",
        concat!("\"calm.plan.upsert\", ", "// retired-name: rejection input"), // retired-name: rejection input
    ] {
        assert!(!offending(miss, &patterns), "must stay green: {miss}");
    }
}

#[test]
fn no_retired_tool_names_remain() {
    let root = workspace_root();
    let patterns = patterns();
    let files = tracked_files(&root);
    assert!(
        files.len() > 500,
        "anti-vacuity: {} tracked files",
        files.len()
    );
    let mut hits = Vec::new();
    for path in files.iter().filter(|path| !allowlisted(path)) {
        let bytes = match std::fs::read(root.join(path)) {
            Ok(bytes) => bytes,
            // A tracked file deleted in the working tree has nothing left to say.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("{path}: {error}"),
        };
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if offending(line, &patterns) {
                hits.push(format!("{path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "retired tool names or server key remain (rename to `neige_<object>_<action>` / \
         `mcp__neige`, or mark a deliberate rejection input with `{REJECTION_INPUT_MARKER}`):\n{}",
        hits.join("\n")
    );
}
