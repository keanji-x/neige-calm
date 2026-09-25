//! The `neige` command table (#1801): argv shape, the `track-gc` keep default and the two `--force`
//! gates. Values reach the tool unchecked; range and non-empty rules belong to the tool alone.

use serde_json::{Map, Value};

use super::help;
use super::render::Render;
use crate::mcp_server::tools::{admin, emit, track_file, track_history, track_state};
use crate::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP;

pub(crate) struct Command {
    pub(crate) name: &'static str,
    pub(crate) tool: &'static str,
    pub(crate) positionals: &'static [Positional],
    /// Refusal for one positional too many; `None` names the unexpected argument.
    pub(crate) too_many: Option<&'static str>,
    pub(crate) options: &'static [Opt],
    pub(crate) confirm: Option<Confirm>,
    pub(crate) render: Render,
}

pub(crate) struct Positional {
    pub(crate) key: &'static str,
    /// `Some(refusal)` makes the positional required.
    pub(crate) missing: Option<&'static str>,
}

pub(crate) struct Opt {
    pub(crate) flag: &'static str,
    pub(crate) key: &'static str,
    pub(crate) value: OptValue,
    pub(crate) required: bool,
}

pub(crate) enum OptValue {
    /// Present means `true`.
    Flag,
    Text,
    Integer {
        default: Option<u64>,
    },
    /// JSON when the text parses as JSON, otherwise the text as a JSON string.
    JsonOrText,
    /// Repeatable; collected into an array.
    TextList,
}

/// A destructive command runs only with `flag`, or with the `unless` tool key set.
pub(crate) struct Confirm {
    pub(crate) flag: &'static str,
    pub(crate) unless: Option<&'static str>,
    pub(crate) refusal: &'static str,
}

const fn opt(flag: &'static str, key: &'static str, value: OptValue, required: bool) -> Opt {
    Opt {
        flag,
        key,
        value,
        required,
    }
}

const fn pos(key: &'static str, missing: Option<&'static str>) -> Positional {
    Positional { key, missing }
}

const FORCE: &str = "--force";

pub(crate) const COMMANDS: &[Command] = &[
    Command {
        name: "ls",
        tool: track_file::TOOL_TRACK_LS,
        positionals: &[pos("path", None)],
        too_many: Some("ls accepts at most one path"),
        options: &[],
        confirm: None,
        render: Render::Ls,
    },
    Command {
        name: "cat",
        tool: track_file::TOOL_TRACK_CAT,
        positionals: &[pos("path", Some("cat requires a path argument"))],
        too_many: Some("cat accepts exactly one path"),
        options: &[],
        confirm: None,
        render: Render::Content,
    },
    Command {
        name: "state",
        tool: track_state::TOOL_TRACK_STATE,
        positionals: &[],
        too_many: Some("state takes no path argument"),
        options: &[],
        confirm: None,
        render: Render::State,
    },
    Command {
        name: "diff",
        tool: track_history::TOOL_TRACK_DIFF,
        positionals: &[
            pos("from", Some("diff requires a from commit")),
            pos("to", None),
            pos("path", None),
        ],
        too_many: Some("diff accepts at most: <from> [to] [path]"),
        options: &[
            opt("--to", "to", OptValue::Text, false),
            opt("--path", "path", OptValue::Text, false),
        ],
        confirm: None,
        render: Render::Diff,
    },
    Command {
        name: "cat-at",
        tool: track_history::TOOL_TRACK_CAT_AT,
        positionals: &[
            pos("commit", Some("cat-at requires <commit> <path>")),
            pos("path", Some("cat-at requires <commit> <path>")),
        ],
        too_many: Some("cat-at requires <commit> <path>"),
        options: &[],
        confirm: None,
        render: Render::Content,
    },
    Command {
        name: "log",
        tool: track_history::TOOL_TRACK_LOG,
        positionals: &[pos("path", None)],
        too_many: Some("log accepts at most one path"),
        options: &[
            opt(
                "--limit",
                "limit",
                OptValue::Integer { default: None },
                false,
            ),
            opt("--include-empty", "include_empty", OptValue::Flag, false),
        ],
        confirm: None,
        render: Render::Log,
    },
    Command {
        name: "task-completed",
        tool: emit::TOOL_TASK_COMPLETE,
        positionals: &[],
        too_many: None,
        options: &[
            opt("--idempotency-key", "idempotency_key", OptValue::Text, true),
            opt("--result", "result", OptValue::JsonOrText, false),
            opt("--artifact", "artifacts", OptValue::TextList, false),
        ],
        confirm: None,
        render: Render::Raw,
    },
    Command {
        name: "task-failed",
        tool: emit::TOOL_TASK_FAIL,
        positionals: &[],
        too_many: None,
        options: &[
            opt("--idempotency-key", "idempotency_key", OptValue::Text, true),
            opt("--reason", "reason", OptValue::Text, true),
        ],
        confirm: None,
        render: Render::Raw,
    },
    Command {
        name: "track-gc",
        tool: admin::TOOL_ADMIN_TRACK_GC,
        positionals: &[],
        too_many: None,
        options: &[
            opt("--track-id", "track_id", OptValue::Text, true),
            opt(
                "--keep",
                "keep",
                OptValue::Integer {
                    default: Some(DEFAULT_TRACK_HISTORY_PRUNE_KEEP as u64),
                },
                false,
            ),
            opt("--dry-run", "dry_run", OptValue::Flag, false),
        ],
        confirm: Some(Confirm {
            flag: FORCE,
            unless: Some("dry_run"),
            refusal: "track-gc is destructive (prunes VCS history + sweeps objects); re-run with --force to confirm",
        }),
        render: Render::Raw,
    },
    Command {
        name: "vacuum",
        tool: admin::TOOL_ADMIN_VACUUM,
        positionals: &[],
        too_many: None,
        options: &[],
        confirm: Some(Confirm {
            flag: FORCE,
            unless: None,
            refusal: "vacuum takes a write lock on the DB and must run in a quiet maintenance window; re-run with --force to confirm",
        }),
        render: Render::Raw,
    },
];

/// One argv mapped onto one tool call.
#[derive(Debug, PartialEq)]
pub(crate) struct Parsed {
    pub(crate) tool: &'static str,
    pub(crate) args: Value,
    pub(crate) json: bool,
    pub(crate) render: Render,
}

/// A refusal before any tool runs; `command` selects the usage line shown with `--json`.
#[derive(Debug, PartialEq)]
pub(crate) struct Usage {
    pub(crate) message: String,
    pub(crate) json: bool,
    pub(crate) command: Option<&'static str>,
}

pub(crate) fn parse(argv: &[String]) -> Result<Parsed, Usage> {
    let mut json = false;
    let mut iter = argv.iter();
    let name = loop {
        match iter.next().map(String::as_str) {
            Some("--json") => json = true,
            Some(name) => break name,
            None => return Err(usage(missing_command(), json, None)),
        }
    };
    if name.starts_with('-') {
        return Err(usage(format!("unknown option `{name}`"), json, None));
    }
    let Some(command) = COMMANDS.iter().find(|c| c.name == name) else {
        return Err(usage(help::unknown_command_message(name), json, None));
    };
    let fail = |message: String, json: bool| usage(message, json, Some(command.name));
    let cmd = command.name;

    let mut args = Map::new();
    let mut positionals = Vec::new();
    let mut forced = false;
    while let Some(arg) = iter.next() {
        if arg == "--json" {
            json = true;
            continue;
        }
        if command.confirm.as_ref().is_some_and(|c| c.flag == arg) {
            forced = true;
            continue;
        }
        if !arg.starts_with('-') {
            positionals.push(arg.clone());
            continue;
        }
        let Some(opt) = command.options.iter().find(|o| o.flag == arg) else {
            return Err(fail(format!("unknown option `{arg}`"), json));
        };
        if let OptValue::Flag = opt.value {
            args.insert(opt.key.into(), Value::Bool(true));
            continue;
        }
        let Some(raw) = iter.next() else {
            return Err(fail(format!("{cmd} requires a value after {arg}"), json));
        };
        let value = match opt.value {
            OptValue::Flag => unreachable!("flags take no value"),
            OptValue::Text => Value::String(raw.clone()),
            OptValue::Integer { .. } => raw
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| fail(format!("{cmd} {arg} must be a non-negative integer"), json))?,
            OptValue::JsonOrText => {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
            }
            OptValue::TextList => {
                let list = args
                    .entry(opt.key)
                    .or_insert_with(|| Value::Array(Vec::new()));
                list.as_array_mut()
                    .expect("list options hold arrays")
                    .push(Value::String(raw.clone()));
                continue;
            }
        };
        if args.insert(opt.key.into(), value).is_some() {
            return Err(fail(format!("{cmd} accepts {arg} once"), json));
        }
    }

    if positionals.len() > command.positionals.len() {
        let extra = &positionals[command.positionals.len()];
        let message = match command.too_many {
            Some(message) => message.to_string(),
            None => format!("unexpected argument `{extra}`"),
        };
        return Err(fail(message, json));
    }
    for (index, spec) in command.positionals.iter().enumerate() {
        match positionals.get(index) {
            Some(value) => {
                if let Some(opt) = command.options.iter().find(|o| o.key == spec.key)
                    && args.contains_key(spec.key)
                {
                    return Err(fail(
                        format!(
                            "{cmd} accepts either positional {} or {}, not both",
                            spec.key, opt.flag
                        ),
                        json,
                    ));
                }
                args.insert(spec.key.into(), Value::String(value.clone()));
            }
            None => {
                if let Some(missing) = spec.missing {
                    return Err(fail(missing.to_string(), json));
                }
            }
        }
    }
    for opt in command.options {
        if opt.required && !args.contains_key(opt.key) {
            return Err(fail(format!("{cmd} requires {}", opt.flag), json));
        }
        if let OptValue::Integer {
            default: Some(default),
        } = opt.value
        {
            args.entry(opt.key).or_insert(Value::from(default));
        }
    }
    if let Some(confirm) = &command.confirm {
        let exempt = confirm
            .unless
            .is_some_and(|key| args.get(key) == Some(&Value::Bool(true)));
        if !forced && !exempt {
            return Err(fail(confirm.refusal.to_string(), json));
        }
    }

    Ok(Parsed {
        tool: command.tool,
        args: Value::Object(args),
        json,
        render: command.render,
    })
}

fn usage(message: String, json: bool, command: Option<&'static str>) -> Usage {
    Usage {
        message,
        json,
        command,
    }
}

fn missing_command() -> String {
    let names: Vec<String> = COMMANDS.iter().map(|c| format!("`{}`", c.name)).collect();
    let (last, rest) = names.split_last().expect("the command table is not empty");
    format!("missing command; expected {}, or {last}", rest.join(", "))
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
