//! #2003 stale-name sweep: no retired kernel tool name and no retired MCP server key survives in a
//! tracked file the product ships or runs, so a prompt, refusal, observation, fixture or fe constant
//! that still says the old name is red here instead of misleading an agent at run time.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The retired kernel tool names: the old `calm.` prefix on any kernel object, plus the removed
/// aliases and shims by their own spelling.
const RETIRED_TOOL_NAME: &str = r"\b(?:calm\.(admin|area|calendar|dispatch_request|get_track_state|plan|preview|ratify|report|review|source|task|task_completed|task_failed|terminal|track|update_task_meta|user)|neige\.track\.publish)\b";

/// The retired MCP server key as a client spells it, assembled from two literals so this file does
/// not carry the token it hunts.
fn retired_server_key() -> String {
    concat!("mcp__", "calm(?:__|\\b)").to_string()
}

/// A deliberate retired-name input (a rejection test) is exempt per line, never per file.
const REJECTION_INPUT_MARKER: &str = "// retired-name: rejection input";

/// The closed allowlist: released migrations and the #2003 migration (one directory, byte-frozen
/// once released), the migration's test, and the design document that records the old names.
fn allowlisted(path: &str) -> bool {
    path.starts_with("crates/calm-truth/migrations/")
        || path == "crates/calm-server/tests/cases/neige_tool_name_migration.rs"
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

fn patterns() -> [regex::Regex; 2] {
    [
        regex::Regex::new(RETIRED_TOOL_NAME).expect("tool-name regex"),
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
        "neige.track.publish",        // retired-name: rejection input
        "`calm.track.cat_at`",        // retired-name: rejection input
        "calm.report.write_markdown", // retired-name: rejection input
        "calm.task_completed",        // retired-name: rejection input
        "\"calm.user.notify\"",       // retired-name: rejection input
        "calm.track: no such path",   // retired-name: rejection input
        concat!("mcp__", "calm__neige_report_read"),
        concat!("allowed: mcp__", "calm Edit"),
    ] {
        assert!(offending(hit, &patterns), "must be red: {hit}");
    }
    for miss in [
        "neige.dev.publish",
        "calm.db",
        "calm_server::mcp_server",
        "calm-server",
        "calm-truth/migrations",
        "neige.track.show",
        "dev.neige.git-forge",
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
        "retired tool names or server key remain (rename to `neige.<object>.<action>` / \
         `mcp__neige`, or mark a deliberate rejection input with `{REJECTION_INPUT_MARKER}`):\n{}",
        hits.join("\n")
    );
}
