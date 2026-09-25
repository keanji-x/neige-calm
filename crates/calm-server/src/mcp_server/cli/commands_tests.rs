//! Parse tests moved from the former fat client (`neige-cli/src/main.rs` unit tests and the argument
//! assertions of `neige-cli/tests/neige_cli.rs`), plus the table-level pins H5 and H8 (#1801).

use super::*;
use crate::mcp_server::build_default_registry;
use crate::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP;
use serde_json::json;
use std::path::{Path, PathBuf};

fn parse_args(args: &[&str]) -> Result<Parsed, Usage> {
    parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

fn tool_args(args: &[&str]) -> Value {
    parse_args(args).expect("parse").args
}

fn refusal(args: &[&str]) -> String {
    parse_args(args).expect_err("must not parse").message
}

#[test]
fn ls_without_a_path_sends_no_path() {
    let parsed = parse_args(&["ls"]).expect("parse");
    assert_eq!(parsed.tool, "calm.track.ls");
    assert_eq!(parsed.args, json!({}));
    assert!(!parsed.json);
    assert_eq!(tool_args(&["ls", "runs/"]), json!({ "path": "runs/" }));
    assert_eq!(refusal(&["ls", "a", "b"]), "ls accepts at most one path");
}

#[test]
fn state_takes_no_arguments() {
    assert_eq!(tool_args(&["state"]), json!({}));
    assert_eq!(refusal(&["state", "extra"]), "state takes no path argument");
}

#[test]
fn token_option_is_not_accepted() {
    assert_eq!(
        refusal(&["--token", "secret", "ls"]),
        "unknown option `--token`"
    );
    assert_eq!(
        refusal(&["ls", "--token", "secret"]),
        "unknown option `--token`"
    );
}

#[test]
fn diff_maps_positionals_to_from_to_and_path() {
    let parsed = parse_args(&["--json", "diff", "abc123", "def456", "report.md"]).expect("parse");
    assert_eq!(parsed.tool, "calm.track.diff");
    assert_eq!(
        parsed.args,
        json!({ "from": "abc123", "to": "def456", "path": "report.md" })
    );
    assert!(parsed.json);
    assert_eq!(refusal(&["diff"]), "diff requires a from commit");
    assert_eq!(
        refusal(&["diff", "a", "b", "c", "d"]),
        "diff accepts at most: <from> [to] [path]"
    );
}

#[test]
fn diff_maps_path_option_without_to() {
    assert_eq!(
        tool_args(&["diff", "abc123", "--path", "report.md"]),
        json!({ "from": "abc123", "path": "report.md" })
    );
}

#[test]
fn diff_rejects_the_same_key_twice() {
    assert_eq!(
        refusal(&["diff", "a", "b", "--to", "c"]),
        "diff accepts either positional to or --to, not both"
    );
    assert_eq!(
        refusal(&["diff", "a", "b", "p", "--path", "q"]),
        "diff accepts either positional path or --path, not both"
    );
    assert_eq!(
        refusal(&["diff", "a", "--to", "b", "--to", "c"]),
        "diff accepts --to once"
    );
    assert_eq!(
        refusal(&["diff", "a", "--to"]),
        "diff requires a value after --to"
    );
}

#[test]
fn cat_at_maps_commit_and_path() {
    assert_eq!(
        tool_args(&["cat-at", "abc123", "report.md"]),
        json!({ "commit": "abc123", "path": "report.md" })
    );
    for args in [&["cat-at", "abc123"][..], &["cat-at", "a", "b", "c"][..]] {
        assert_eq!(refusal(args), "cat-at requires <commit> <path>");
    }
    assert_eq!(refusal(&["cat"]), "cat requires a path argument");
    assert_eq!(refusal(&["cat", "a", "b"]), "cat accepts exactly one path");
}

#[test]
fn log_maps_path_limit_and_include_empty() {
    let parsed =
        parse_args(&["log", "report.md", "--limit", "7", "--include-empty"]).expect("parse");
    assert_eq!(
        parsed.args,
        json!({ "path": "report.md", "limit": 7, "include_empty": true })
    );
    assert!(!parsed.json);
    assert_eq!(
        refusal(&["log", "--limit", "seven"]),
        "log --limit must be a non-negative integer"
    );
}

/// H4: range and non-empty rules belong to the tool (log clamps 0 to 1, an empty `to`/`path` is none,
/// a blank reason is refused by `calm.task.fail`).
#[test]
fn values_reach_the_tool_unchecked() {
    assert_eq!(tool_args(&["log", "--limit", "0"]), json!({ "limit": 0 }));
    assert_eq!(
        tool_args(&["diff", "a", "--to", "", "--path", ""]),
        json!({ "from": "a", "to": "", "path": "" })
    );
    assert_eq!(
        tool_args(&["task-failed", "--idempotency-key", "", "--reason", " "]),
        json!({ "idempotency_key": "", "reason": " " })
    );
    assert_eq!(
        tool_args(&["track-gc", "--track-id", "", "--keep", "0", "--force"]),
        json!({ "track_id": "", "keep": 0 })
    );
}

#[test]
fn track_gc_defaults_keep_to_the_prune_constant() {
    assert_eq!(
        tool_args(&["track-gc", "--track-id", "w-1", "--force"]),
        json!({ "track_id": "w-1", "keep": DEFAULT_TRACK_HISTORY_PRUNE_KEEP })
    );
    let parsed = parse_args(&[
        "track-gc",
        "--track-id",
        "w-1",
        "--keep",
        "10",
        "--dry-run",
        "--json",
    ])
    .expect("dry-run parses without --force");
    assert_eq!(
        parsed.args,
        json!({ "track_id": "w-1", "keep": 10, "dry_run": true })
    );
    assert!(parsed.json);
}

#[test]
fn track_gc_requires_track_id() {
    assert_eq!(
        refusal(&["track-gc", "--force"]),
        "track-gc requires --track-id"
    );
}

#[test]
fn track_gc_requires_force_unless_dry_run() {
    assert!(refusal(&["track-gc", "--track-id", "w-1"]).contains("re-run with --force to confirm"));
    assert!(parse_args(&["track-gc", "--track-id", "w-1", "--dry-run"]).is_ok());
}

#[test]
fn vacuum_requires_force() {
    assert!(refusal(&["vacuum"]).contains("re-run with --force to confirm"));
    let parsed = parse_args(&["vacuum", "--force", "--json"]).expect("parse");
    assert_eq!(parsed.tool, "calm.admin.vacuum");
    assert_eq!(parsed.args, json!({}));
    assert!(parsed.json);
    assert_eq!(
        refusal(&["vacuum", "--force", "now"]),
        "unexpected argument `now`"
    );
}

#[test]
fn task_completed_parses_json_result_and_artifacts() {
    let parsed = parse_args(&[
        "task-completed",
        "--idempotency-key",
        "k1",
        "--result",
        r#"{"ok":true}"#,
        "--artifact",
        "out.log",
        "--artifact",
        "b.txt",
        "--json",
    ])
    .expect("parse");
    assert_eq!(
        parsed.args,
        json!({ "idempotency_key": "k1", "result": { "ok": true }, "artifacts": ["out.log", "b.txt"] })
    );
    assert!(parsed.json);
}

#[test]
fn task_completed_keeps_plain_text_result_as_a_string() {
    assert_eq!(
        tool_args(&[
            "task-completed",
            "--idempotency-key",
            "k1",
            "--result",
            "plain text"
        ]),
        json!({ "idempotency_key": "k1", "result": "plain text" })
    );
    assert_eq!(
        refusal(&["task-completed"]),
        "task-completed requires --idempotency-key"
    );
}

#[test]
fn task_failed_requires_reason() {
    assert_eq!(
        refusal(&["task-failed", "--idempotency-key", "k1"]),
        "task-failed requires --reason"
    );
}

#[test]
fn json_flag_is_accepted_before_and_after_the_command() {
    for args in [
        &["--json", "state"][..],
        &["state", "--json"][..],
        &["--json", "--json", "state"][..],
    ] {
        assert!(parse_args(args).expect("parse").json, "{args:?}");
    }
    let err = parse_args(&["--json", "snow"]).expect_err("unknown command");
    assert!(err.json);
    assert!(err.message.starts_with("unknown command `snow`"), "{err:?}");
    assert!(refusal(&[]).starts_with("missing command; expected `ls`, `cat`"));
}

/// H5: every argv slot names a property of its tool's input schema, and a slot the CLI requires is one
/// the schema requires. `--json`, `--force` and `-h/--help` carry no tool argument and are not slots.
#[test]
fn every_cli_option_maps_to_a_tool_schema_property() {
    let descriptors = build_default_registry().descriptors();
    for command in COMMANDS {
        let descriptor = descriptors
            .iter()
            .find(|d| d.name == command.tool)
            .unwrap_or_else(|| panic!("{} maps to unregistered {}", command.name, command.tool));
        let schema = &descriptor.input_schema;
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|keys| keys.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let slots = command
            .positionals
            .iter()
            .map(|p| (p.key, p.missing.is_some()))
            .chain(command.options.iter().map(|o| (o.key, o.required)));
        for (key, cli_required) in slots {
            assert!(
                schema["properties"].get(key).is_some(),
                "{} slot `{key}` is not a {} schema property",
                command.name,
                command.tool
            );
            assert!(
                !cli_required || required.contains(&key),
                "{} requires `{key}` but {} does not",
                command.name,
                command.tool
            );
        }
        for opt in command.options {
            assert!(
                !matches!(opt.flag, "--json" | "--force" | "-h" | "--help"),
                "{}",
                opt.flag
            );
        }
    }
}

#[test]
fn help_documents_exactly_the_served_commands() {
    let mut documented: Vec<String> = help::available_commands()
        .split(", ")
        .map(str::to_string)
        .collect();
    assert_eq!(documented.pop().as_deref(), Some("help"));
    let served: Vec<String> = COMMANDS.iter().map(|c| c.name.to_string()).collect();
    assert_eq!(documented, served);
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// H8: every `` `neige <command>`` in agent-facing prose names a command this table serves, so a rename fails here first.
#[test]
fn prompt_neige_mentions_name_served_commands() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["prompts", "templates/builtin", "../calm-types/src/report"] {
        markdown_files(&crate_dir.join(dir), &mut files);
    }
    let mut mentions = 0;
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read prompt");
        for (index, _) in text.match_indices("`neige ") {
            let word: String = text[index + "`neige ".len()..]
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if word.is_empty() {
                continue;
            }
            mentions += 1;
            assert!(
                COMMANDS.iter().any(|c| c.name == word),
                "{} mentions `neige {word}`, which the kernel does not serve",
                file.display()
            );
        }
    }
    assert!(
        mentions >= 10,
        "scan is vacuous: {mentions} mentions in {} files",
        files.len()
    );
}
