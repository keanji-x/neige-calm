//! Parse tests moved from the former fat client (`neige-cli/src/main.rs` unit tests and the argument
//! assertions of `neige-cli/tests/neige_cli.rs`), plus the table-level pins H5 and H8 (#1801).

use super::*;
use crate::mcp_server::build_default_registry;
use serde_json::json;
use std::path::{Path, PathBuf};

static REGISTRY: std::sync::LazyLock<std::sync::Arc<ToolRegistry>> =
    std::sync::LazyLock::new(build_default_registry);

fn parse_args(args: &[&str]) -> Result<Parsed, Usage> {
    parse(
        &args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>(),
        &REGISTRY,
    )
}

fn tool_args(args: &[&str]) -> Value {
    parse_args(args).expect("parse").args
}

fn refusal(args: &[&str]) -> String {
    parse_args(args).expect_err("must not parse").message
}

#[test]
fn ls_without_a_path_sends_no_path() {
    let parsed = parse_args(&["track", "ls"]).expect("parse");
    assert_eq!(parsed.tool, "neige_track_ls");
    assert_eq!(parsed.args, json!({}));
    assert!(!parsed.json);
    assert_eq!(
        tool_args(&["track", "ls", "runs/"]),
        json!({ "path": "runs/" })
    );
    assert_eq!(
        refusal(&["track", "ls", "a", "b"]),
        "unexpected argument `b`; usage: neige track ls [path] [-l] [--json]"
    );
}

/// #1838: `-l` shapes only the text and never reaches the tool; `area/reports/` selects the report table.
#[test]
fn ls_long_is_a_view_flag_and_area_reports_selects_the_report_listing() {
    for (argv, long, reports) in [
        (&["track", "ls"][..], false, false),
        (&["track", "ls", "-l"][..], true, false),
        (&["track", "ls", "-l", "area/reports/"][..], true, true),
        (&["track", "ls", "/area/reports", "-l"][..], true, true),
        (&["track", "ls", "area/reports"][..], false, true),
        (&["track", "ls", "-l", "area/"][..], true, false),
        (&["track", "ls", "area/reports/x.md"][..], false, false),
    ] {
        let parsed = parse_args(argv).expect("parse");
        assert_eq!(parsed.render, Render::Ls { long, reports }, "{argv:?}");
        assert!(parsed.args.get("long").is_none(), "{argv:?}");
    }
    assert_eq!(
        tool_args(&["track", "ls", "-l", "runs/"]),
        json!({ "path": "runs/" })
    );
    assert_eq!(
        refusal(&["track", "ls", "-a"]),
        "unknown option `-a` for `neige track ls`; expected one of: -l, --path, --json"
    );
}

#[test]
fn find_maps_path_name_and_tag_each_at_most_once() {
    let parsed = parse_args(&[
        "report",
        "find",
        "area/reports/",
        "--name",
        "*认证*",
        "--tag",
        "认证",
    ])
    .expect("parse");
    assert_eq!(parsed.tool, "neige_report_find");
    assert_eq!(parsed.render, Render::Find);
    assert_eq!(
        parsed.args,
        json!({ "path": "area/reports/", "name": "*认证*", "tag": "认证" })
    );
    assert_eq!(
        tool_args(&["report", "find", "--tag", "-x", "area/reports"]),
        json!({ "path": "area/reports", "tag": "-x" }),
        "an option value is taken verbatim, even one starting with `-`"
    );
    assert_eq!(refusal(&["report", "find"]), "report find requires <path>");
    assert_eq!(
        refusal(&[
            "report",
            "find",
            "area/reports/",
            "--tag",
            "a",
            "--tag",
            "b"
        ]),
        "report find accepts --tag once"
    );
    assert_eq!(
        refusal(&[
            "report",
            "find",
            "area/reports/",
            "--name",
            "a",
            "--name",
            "b"
        ]),
        "report find accepts --name once"
    );
    assert_eq!(
        refusal(&["report", "find", "area/reports/", "-name", "f"]),
        "unknown option `-name` for `neige report find`; expected one of: --name, --tag, --path, --json"
    );
    assert_eq!(
        refusal(&["report", "find", "a", "b"]),
        "unexpected argument `b`; usage: neige report find area/reports/ [--name <glob>] [--tag <tag>] [--json]"
    );
}

/// #1874: `--blocks` is the comma-separated CLI spelling of `neige_report_read`'s `blocks`; the pieces reach the
/// tool verbatim (an empty piece is the tool's to refuse).
#[test]
fn cat_blocks_sends_the_comma_separated_ids_as_an_array() {
    let parsed = parse_args(&[
        "track",
        "cat",
        "area/reports/认证 方案.md",
        "--blocks",
        "b_1,b_2",
    ])
    .expect("parse");
    assert_eq!(parsed.tool, "neige_track_cat");
    assert_eq!(parsed.render, Render::Content);
    assert_eq!(
        parsed.args,
        json!({ "path": "area/reports/认证 方案.md", "blocks": ["b_1", "b_2"] })
    );
    assert_eq!(
        tool_args(&["track", "cat", "--blocks", "b_1", "report.md"]),
        json!({ "path": "report.md", "blocks": ["b_1"] })
    );
    assert_eq!(
        tool_args(&["track", "cat", "report.md", "--blocks", "b_1,,"]),
        json!({ "path": "report.md", "blocks": ["b_1", "", ""] })
    );
    assert_eq!(
        tool_args(&["track", "cat", "report.md"]),
        json!({ "path": "report.md" })
    );
    assert_eq!(
        refusal(&["track", "cat", "report.md", "--blocks"]),
        "track cat requires a value after --blocks"
    );
    assert_eq!(
        refusal(&[
            "track",
            "cat",
            "report.md",
            "--blocks",
            "b_1",
            "--blocks",
            "b_2"
        ]),
        "track cat accepts --blocks once"
    );
    assert_eq!(
        tool_args(&[
            "track",
            "cat",
            "report.md",
            "--sections",
            "已完成,Next steps"
        ]),
        json!({ "path": "report.md", "sections": ["已完成", "Next steps"] })
    );
}

#[test]
fn state_takes_no_arguments() {
    assert_eq!(tool_args(&["track", "status"]), json!({}));
    assert_eq!(
        refusal(&["track", "status", "extra"]),
        "unexpected argument `extra`; usage: neige track status [--json]"
    );
}

#[test]
fn token_option_is_not_accepted() {
    assert_eq!(
        refusal(&["--token", "secret", "track", "ls"]),
        help::unknown_command_message("--token")
    );
    assert_eq!(
        refusal(&["track", "ls", "--token", "secret"]),
        "unknown option `--token` for `neige track ls`; expected one of: -l, --path, --json"
    );
}

#[test]
fn diff_maps_positionals_to_from_to_and_path() {
    let parsed =
        parse_args(&["--json", "track", "diff", "abc123", "def456", "report.md"]).expect("parse");
    assert_eq!(parsed.tool, "neige_track_diff");
    assert_eq!(
        parsed.args,
        json!({ "from": "abc123", "to": "def456", "path": "report.md" })
    );
    assert!(parsed.json);
    assert_eq!(refusal(&["track", "diff"]), "track diff requires <from>");
    assert_eq!(
        refusal(&["track", "diff", "a", "b", "c", "d"]),
        "unexpected argument `d`; usage: neige track diff <from> [to] [path] [--json]"
    );
}

#[test]
fn diff_maps_path_option_without_to() {
    assert_eq!(
        tool_args(&["track", "diff", "abc123", "--path", "report.md"]),
        json!({ "from": "abc123", "path": "report.md" })
    );
}

#[test]
fn diff_rejects_the_same_key_twice() {
    assert_eq!(
        refusal(&["track", "diff", "a", "b", "p", "--path", "q"]),
        "unexpected argument `p`; usage: neige track diff <from> [to] [path] [--json]"
    );
    assert_eq!(
        refusal(&["track", "diff", "a", "--to", "b", "--to", "c"]),
        "track diff accepts --to once"
    );
    assert_eq!(
        refusal(&["track", "diff", "a", "--to"]),
        "track diff requires a value after --to"
    );
}

#[test]
fn cat_at_maps_commit_and_path() {
    assert_eq!(
        tool_args(&["track", "show", "abc123", "report.md"]),
        json!({ "commit": "abc123", "path": "report.md" })
    );
    assert_eq!(
        refusal(&["track", "show", "abc123"]),
        "track show requires <path>"
    );
    assert_eq!(
        refusal(&["track", "show", "a", "b", "c"]),
        "unexpected argument `c`; usage: neige track show <commit> <path> [--json]"
    );
    assert_eq!(refusal(&["track", "cat"]), "track cat requires <path>");
    assert!(
        refusal(&["track", "cat", "a", "b"])
            .starts_with("unexpected argument `b`; usage: neige track cat <path>")
    );
}

#[test]
fn tag_maps_path_and_repeated_add_and_remove_in_order() {
    let parsed = parse_args(&["report", "tag", "report.md"]).expect("parse");
    assert_eq!(parsed.tool, "neige_report_tag");
    assert_eq!(parsed.render, Render::Tags);
    assert_eq!(parsed.args, json!({ "path": "report.md" }));
    assert_eq!(
        tool_args(&[
            "report",
            "tag",
            "report.md",
            "--add",
            "认证",
            "--remove",
            "排障",
            "--add",
            "架构"
        ]),
        json!({ "path": "report.md", "add": ["认证", "架构"], "remove": ["排障"] })
    );
    // Values reach the tool unchecked: the path and tag rules belong to `neige_report_tag`.
    assert_eq!(
        tool_args(&["report", "tag", "track.json", "--add", " a,b "]),
        json!({ "path": "track.json", "add": [" a,b "] })
    );
    assert_eq!(refusal(&["report", "tag"]), "report tag requires <path>");
    assert_eq!(
        refusal(&["report", "tag", "report.md", "other.md"]),
        "unexpected argument `other.md`; usage: neige report tag <path> [--add <tag>]... [--remove <tag>]... [--json]"
    );
    assert_eq!(
        refusal(&["report", "tag", "report.md", "--add"]),
        "report tag requires a value after --add"
    );
    assert_eq!(
        refusal(&["report", "tag", "report.md", "--track-id", "t"]),
        "unknown option `--track-id` for `neige report tag`; expected one of: --add, --remove, --path, --json"
    );
}

#[test]
fn log_maps_path_cursor_and_include_empty() {
    let parsed = parse_args(&[
        "track",
        "log",
        "report.md",
        "--cursor",
        "abc123",
        "--include-empty",
    ])
    .expect("parse");
    assert_eq!(
        parsed.args,
        json!({ "path": "report.md", "cursor": "abc123", "include_empty": true })
    );
    assert!(!parsed.json);
    assert_eq!(
        refusal(&["track", "log", "--limit", "7"]),
        "unknown option `--limit` for `neige track log`; expected one of: --cursor, --include-empty, --path, --json"
    );
}

/// H4: range and non-empty rules belong to the tool (an unminted cursor is refused by the tool, an
/// empty `to`/`path` is none, a blank reason is refused by `neige_task_fail`).
#[test]
fn values_reach_the_tool_unchecked() {
    assert_eq!(
        tool_args(&["track", "log", "--cursor", ""]),
        json!({ "cursor": "" })
    );
    assert_eq!(
        tool_args(&["track", "diff", "a", "--to", "", "--path", ""]),
        json!({ "from": "a", "to": "", "path": "" })
    );
    assert_eq!(
        tool_args(&["task", "fail", "--attempt-id", "", "--reason", " "]),
        json!({ "attempt_id": "", "reason": " " })
    );
    assert_eq!(
        tool_args(&["admin", "gc", "--track-id", "", "--keep", "0", "--force"]),
        json!({ "track_id": "", "keep": 0 })
    );
}

/// `keep` is required by `neige_admin_gc`, so the CLI mirrors it and supplies no default.
#[test]
fn admin_gc_requires_keep_as_its_tool_does() {
    assert_eq!(
        refusal(&["admin", "gc", "--track-id", "w-1", "--force"]),
        "admin gc requires --keep"
    );
    let parsed = parse_args(&[
        "admin",
        "gc",
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
        refusal(&["admin", "gc", "--force"]),
        "admin gc requires --track-id"
    );
}

#[test]
fn track_gc_requires_force_unless_dry_run() {
    assert!(
        refusal(&["admin", "gc", "--track-id", "w-1", "--keep", "5"])
            .contains("re-run with --force to confirm")
    );
    assert!(
        parse_args(&[
            "admin",
            "gc",
            "--track-id",
            "w-1",
            "--keep",
            "5",
            "--dry-run"
        ])
        .is_ok()
    );
}

#[test]
fn vacuum_requires_force() {
    assert!(refusal(&["admin", "vacuum"]).contains("re-run with --force to confirm"));
    let parsed = parse_args(&["admin", "vacuum", "--force", "--json"]).expect("parse");
    assert_eq!(parsed.tool, "neige_admin_vacuum");
    assert_eq!(parsed.args, json!({}));
    assert!(parsed.json);
    assert_eq!(
        refusal(&["admin", "vacuum", "--force", "now"]),
        "unexpected argument `now`; usage: neige admin vacuum --force [--json]"
    );
}

#[test]
fn task_completed_parses_json_result_and_artifacts() {
    let parsed = parse_args(&[
        "task",
        "done",
        "--attempt-id",
        "k1",
        "--result",
        r#"{"ok":true}"#,
        "--artifacts",
        "out.log",
        "--artifacts",
        "b.txt",
        "--json",
    ])
    .expect("parse");
    assert_eq!(
        parsed.args,
        json!({ "attempt_id": "k1", "result": { "ok": true }, "artifacts": ["out.log", "b.txt"] })
    );
    assert!(parsed.json);
}

#[test]
fn task_completed_keeps_plain_text_result_as_a_string() {
    assert_eq!(
        tool_args(&[
            "task",
            "done",
            "--attempt-id",
            "k1",
            "--result",
            "plain text"
        ]),
        json!({ "attempt_id": "k1", "result": "plain text" })
    );
    assert_eq!(
        refusal(&["task", "done"]),
        "task done requires --attempt-id"
    );
}

/// #2139: `--commit-message` is the tool's `commit_message`, passed as written (newlines and
/// trailer-shaped lines included); the kernel, not the CLI, validates it.
#[test]
fn task_done_maps_commit_message() {
    let message = "fix(forge): x\n\nOWNERSHIP-CHANGE: fe/a.ts — why (#1)\n";
    assert_eq!(
        tool_args(&[
            "task",
            "done",
            "--attempt-id",
            "k1",
            "--commit-message",
            message
        ]),
        json!({ "attempt_id": "k1", "commit_message": message })
    );
}

#[test]
fn task_failed_requires_reason() {
    assert_eq!(
        refusal(&["task", "fail", "--attempt-id", "k1"]),
        "task fail requires --reason"
    );
}

#[test]
fn track_close_maps_the_message_onto_neige_track_close() {
    let parsed = parse_args(&["track", "close", "--message", "goal met"]).expect("parse");
    assert_eq!(parsed.tool, "neige_track_close");
    assert_eq!(parsed.args, json!({ "message": "goal met" }));
    assert_eq!(
        refusal(&["track", "close"]),
        "track close requires --message"
    );
}

#[test]
fn json_flag_is_accepted_before_and_after_the_command() {
    for args in [
        &["--json", "track", "status"][..],
        &["track", "--json", "status"][..],
        &["track", "status", "--json"][..],
        &["--json", "--json", "track", "status"][..],
    ] {
        assert!(parse_args(args).expect("parse").json, "{args:?}");
    }
    let err = parse_args(&["--json", "snow"]).expect_err("unknown command");
    assert!(err.json);
    assert!(err.message.starts_with("unknown command `snow`"), "{err:?}");
    assert_eq!(
        refusal(&[]),
        "missing command; expected `neige <object> <action>` with an object of: track, report, mail, task, admin, tool"
    );
}

/// #2003: an old one-word spelling is no alias: it is an unknown command that lists the objects.
#[test]
fn an_old_spelling_is_a_usage_error_listing_the_objects() {
    for old in [
        "cat",
        "ls",
        "state",
        "find",
        "tag",
        "cat-at",
        "task-completed",
        "task-report-success",
        "task-report-failure",
        "track-gc",
        "vacuum",
    ] {
        let err = parse_args(&[old, "x"]).expect_err("old spellings do not parse");
        assert_eq!(err.command, None, "{old}");
        assert_eq!(err.message, help::unknown_command_message(old), "{old}");
    }
    assert_eq!(
        help::unknown_command_message("cat"),
        "unknown command `cat`; a command is `neige <object> <action>`\n\n\
         Objects: track, report, mail, task, admin, tool\nRun `neige --help` for usage."
    );
    assert_eq!(
        refusal(&["track", "cat-at", "c", "p"]),
        "unknown action `cat-at` for `neige track`; expected one of: ls, cat, show, diff, log, status, close"
    );
    assert_eq!(
        refusal(&["track"]),
        "`neige track` needs an action: ls, cat, show, diff, log, status, close"
    );
}

/// #2289 D3: `neige <object> <action>` naming a kernel tool without a command is refused as
/// MCP-only, on an unknown object and on an unknown action alike; any other unknown spelling keeps
/// its message and choices.
#[test]
fn a_kernel_tool_without_a_command_is_refused_as_mcp_only() {
    for (args, tool) in [
        (
            &["report", "read", "--path", "report.md"][..],
            "neige_report_read",
        ),
        (&["--json", "workspace", "ls"][..], "neige_workspace_ls"),
        (&["track", "rename"][..], "neige_track_rename"),
    ] {
        let err = parse_args(args).expect_err("no CLI command");
        assert_eq!(
            (err.message.as_str(), err.command, err.json),
            (
                format!(
                    "`{tool}` has no CLI command; call it as an MCP tool (`neige tool describe --name {tool}` shows it)"
                )
                .as_str(),
                None,
                args[0] == "--json"
            ),
            "{args:?}"
        );
    }
    assert_eq!(
        refusal(&["report", "nope"]),
        help::unknown_action_message("report", "nope")
    );
    assert_eq!(
        refusal(&["workspace", "nope"]),
        help::unknown_command_message("workspace")
    );
    assert_eq!(
        refusal(&["workspace"]),
        help::unknown_command_message("workspace")
    );
}

/// #2003 §4.4: every positional is also accepted as its `--<key>` option. A named option claims
/// its slot, and the remaining positionals fill the unclaimed slots in order.
#[test]
fn every_positional_is_also_its_option() {
    assert_eq!(
        tool_args(&["track", "cat", "--path", "report.md", "--blocks", "b_1"]),
        json!({ "path": "report.md", "blocks": ["b_1"] })
    );
    assert_eq!(
        tool_args(&["track", "show", "--commit", "c", "--path", "p"]),
        json!({ "commit": "c", "path": "p" })
    );
    assert_eq!(
        tool_args(&["track", "diff", "--from", "a", "--to", "b"]),
        json!({ "from": "a", "to": "b" })
    );
    for argv in [
        &["track", "show", "--commit", "c", "report.md"][..],
        &["track", "show", "report.md", "--commit", "c"][..],
    ] {
        assert_eq!(
            tool_args(argv),
            json!({ "commit": "c", "path": "report.md" }),
            "{argv:?}"
        );
    }
    for argv in [
        &["track", "diff", "--from", "a", "b"][..],
        &["track", "diff", "b", "--from", "a"][..],
    ] {
        assert_eq!(
            tool_args(argv),
            json!({ "from": "a", "to": "b" }),
            "{argv:?}"
        );
    }
    assert_eq!(
        tool_args(&["track", "diff", "a", "p", "--to", "b"]),
        json!({ "from": "a", "to": "b", "path": "p" })
    );
    assert_eq!(
        refusal(&["track", "cat", "a", "--path", "b"]),
        "unexpected argument `a`; usage: neige track cat <path> [--blocks <id,...> | --sections <heading,...>] [--json]"
    );
    for command in COMMANDS {
        for slot in command.positionals {
            let mut argv: Vec<String> = command.spelling().split(' ').map(str::to_string).collect();
            for other in command.positionals {
                argv.extend([option_flag(other.key), format!("v-{}", other.key)]);
            }
            for opt in command.options.iter().filter(|o| o.required) {
                argv.extend([opt.flag.to_string(), "v".into()]);
            }
            let parsed = parse(&argv, &REGISTRY).unwrap_or_else(|e| panic!("{argv:?}: {e:?}"));
            assert_eq!(
                parsed.args[slot.key],
                json!(format!("v-{}", slot.key)),
                "{argv:?}"
            );
        }
    }
}

/// #2003 §4.4 (H5): every option is `--<key>` with `_` written `-`, and every option and positional
/// key is a property of the tool's input schema; a slot the CLI requires is one the schema requires.
/// Only a view flag (`track ls -l`) has its own spelling, and it never reaches the tool.
#[test]
fn every_option_is_its_schema_key() {
    let descriptors = build_default_registry().descriptors();
    for command in COMMANDS {
        let name = command.spelling();
        let descriptor = descriptors
            .iter()
            .find(|d| d.name == command.tool)
            .unwrap_or_else(|| panic!("{name} maps to unregistered {}", command.tool));
        let schema = &descriptor.input_schema;
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|keys| keys.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for opt in command
            .options
            .iter()
            .filter(|o| !matches!(o.value, OptValue::View))
        {
            assert_eq!(opt.flag, option_flag(opt.key), "{name}");
        }
        let slots = command
            .positionals
            .iter()
            .map(|p| (p.key, p.required))
            .chain(
                command
                    .options
                    .iter()
                    .filter(|o| !matches!(o.value, OptValue::View))
                    .map(|o| (o.key, o.required)),
            );
        for (key, cli_required) in slots {
            assert!(
                schema["properties"].get(key).is_some(),
                "{name} slot `{key}` is not a {} schema property",
                command.tool
            );
            assert!(
                !cli_required || required.contains(&key),
                "{name} requires `{key}` but {} does not",
                command.tool
            );
        }
        let flags = accepted_flags(command);
        let mut unique = flags.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), flags.len(), "{name}: a flag is spelled twice");
        for opt in command.options {
            assert!(
                !matches!(opt.flag, "--json" | "--force" | "-h" | "--help"),
                "{}",
                opt.flag
            );
        }
    }
}

/// The grammar of §4.2: the spelling is the tool's two words, and no two rows share a tool.
#[test]
fn every_command_is_spelled_from_its_tool() {
    let mut tools: Vec<&str> = COMMANDS.iter().map(|c| c.tool).collect();
    for command in COMMANDS {
        let (object, action) = command.words();
        assert_eq!(command.tool, format!("neige_{object}_{action}"));
        assert_eq!(
            cli_spelling(command.tool),
            Some(format!("neige {object} {action}"))
        );
    }
    tools.sort();
    tools.dedup();
    assert_eq!(tools.len(), COMMANDS.len());
    assert_eq!(cli_spelling("neige_track_rename"), None);
    // #2087 B0: every action is one word, so the CLI is the tool name split at `_`.
    assert_eq!(
        cli_spelling("neige_task_done").as_deref(),
        Some("neige task done")
    );
    assert_eq!(
        cli_spelling("neige_task_fail").as_deref(),
        Some("neige task fail")
    );
}

/// Every served command has help whose Usage line starts with its spelling and names every flag.
#[test]
fn help_documents_exactly_the_served_commands() {
    let documented: Vec<String> = help::available_commands()
        .split(", ")
        .map(str::to_string)
        .collect();
    let served: Vec<String> = COMMANDS
        .iter()
        .map(Command::spelling)
        .chain(["tool ls|describe".to_string(), "help".to_string()])
        .collect();
    assert_eq!(documented, served);
    for command in COMMANDS {
        let (object, action) = command.words();
        let text = help::render(help::HelpRequest::Command(object, action)).expect("help");
        assert!(
            help::usage_line(Some(command.tool)).starts_with(&format!("neige {object} {action}")),
            "{text}"
        );
        for flag in accepted_flags(command) {
            assert!(
                text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .any(|word| word == flag),
                "`neige {object} {action}` help does not name {flag}"
            );
        }
        let object_help = help::render(help::HelpRequest::Object(object)).expect("object help");
        assert!(
            object_help.contains(&format!("  {action} ")),
            "{object_help}"
        );
    }
    assert!(help::render(help::HelpRequest::Object("cat")).is_none());
    assert!(help::render(help::HelpRequest::Command("track", "cat-at")).is_none());
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

/// H8: every `` `neige <object> <action>`` in agent-facing prose names a command the kernel serves,
/// with only options it accepts, so a rename fails here first. The prose is every markdown file
/// the kernel embeds for agents: prompts, built-in templates, every built-in plugin's guide
/// (#2139), report bodies and observation texts. The frozen pre-header report body keeps its
/// shipped bytes.
#[test]
fn prompt_neige_mentions_name_served_commands() {
    const FROZEN: &str = "../calm-types/src/report/legacy_initial_v4.md";
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [
        "prompts",
        "templates/builtin",
        "src/builtin_plugins",
        "../calm-types/src/report",
        "../calm-types/src/observation",
    ] {
        markdown_files(&crate_dir.join(dir), &mut files);
    }
    assert!(
        files.contains(&crate_dir.join(FROZEN)),
        "allow-list names a missing file"
    );
    files.retain(|file| *file != crate_dir.join(FROZEN));
    let word = |text: &str| -> String {
        text.chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '-')
            .collect()
    };
    let mut mentions = 0;
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read prompt");
        for (index, _) in text.match_indices("`neige ") {
            let rest = &text[index + "`neige ".len()..];
            let object = word(rest);
            if object.is_empty() || object == "help" {
                continue;
            }
            mentions += 1;
            let action = rest[object.len()..]
                .strip_prefix(' ')
                .map(word)
                .unwrap_or_default();
            assert!(
                objects().contains(&object.as_str()) && actions(&object).contains(&action),
                "{} mentions `neige {object} {action}`, which the kernel does not serve",
                file.display()
            );
            let command = COMMANDS.iter().find(|c| c.is(&object, &action));
            let Some(command) = command else {
                continue;
            };
            let span = rest.split('`').next().unwrap_or_default();
            for flag in span
                .split_whitespace()
                .map(|token| token.trim_matches(|c: char| "[]().,|".contains(c)))
                .filter(|token| token.starts_with('-'))
            {
                assert!(
                    accepted_flags(command)
                        .iter()
                        .any(|accepted| accepted == flag),
                    "{} spells `neige {object} {action} {flag}`, an option it does not accept",
                    file.display()
                );
            }
        }
    }
    assert!(
        mentions >= 10,
        "scan is vacuous: {mentions} mentions in {} files",
        files.len()
    );
}

/// #1944: the task execution id has one agent-facing name, `attempt_id`. No prompt or CLI help may
/// call it an idempotency key, a kernel task id or a terminal `task_id` again.
#[test]
fn task_report_surfaces_name_the_execution_id_attempt_id() {
    // Where `idempotency_key` is a real caller-chosen dedupe key, not a task execution id.
    const CALLER_DEDUPE_KEY_PROMPTS: [&str; 6] = [
        "prompts/tools/plugin_calendar_add.md",
        "prompts/tools/plugin_gitforge_publish.md",
        "prompts/tools/neige_terminal_open.md",
        "prompts/tools/neige_terminal_input.md",
        "prompts/guides/terminal.md",
        "prompts/tools/neige_track_add.md",
    ];
    const RETIRED_NAMES: [&str; 5] = [
        "idempotency_key",
        "idempotency-key",
        "idempotency key",
        "kernel task id",
        "task_id",
    ];
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    markdown_files(&crate_dir.join("prompts"), &mut files);
    for allowed in CALLER_DEDUPE_KEY_PROMPTS {
        assert!(
            files.contains(&crate_dir.join(allowed)),
            "allow-list names a missing prompt: {allowed}"
        );
    }
    files.retain(|file| {
        !CALLER_DEDUPE_KEY_PROMPTS
            .iter()
            .any(|allowed| *file == crate_dir.join(allowed))
    });
    files.push(crate_dir.join("src/mcp_server/cli/help.rs"));
    assert!(files.len() >= 40, "scan is vacuous: {} files", files.len());
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
            .to_lowercase();
        for name in RETIRED_NAMES {
            assert!(
                !text.contains(name),
                "{} calls the task execution id `{name}`; name it `attempt_id`",
                file.display()
            );
        }
    }
}
