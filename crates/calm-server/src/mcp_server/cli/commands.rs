//! The `neige` command table (#1801, #2003): `neige <object> <action>` spelled from each row's tool,
//! `--<key>` options, the `admin gc` keep default and the two `--force` gates.
//! Values reach the tool unchecked; range and non-empty rules belong to the tool alone.

use serde_json::{Map, Value};

use super::help;
use super::render::Render;
use crate::area_reports::{self, AreaPath};
use crate::mcp_server::tools::{
    admin, area_reports as area_reports_tool, emit, report_tag, track_file, track_history,
    track_state,
};
use crate::track_fs_view::normalize_path;
use crate::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP;

/// One CLI command: `neige <object> <action>`, the tool name `neige_<object>_<action>` split at `_`.
pub(crate) struct Command {
    pub(crate) tool: &'static str,
    pub(crate) positionals: &'static [Positional],
    pub(crate) options: &'static [Opt],
    pub(crate) confirm: Option<Confirm>,
    pub(crate) render: Render,
}

impl Command {
    /// `(object, action)` as typed: the two words of `tool` after `neige_`.
    pub(crate) fn words(&self) -> (&'static str, &'static str) {
        tool_words(self.tool).expect("every CLI tool is neige_<object>_<action>")
    }

    /// Whether `neige <object> <action>` names this command.
    pub(crate) fn is(&self, object: &str, action: &str) -> bool {
        let (own_object, own_action) = self.words();
        own_object == object && own_action == action
    }

    /// `track cat` for `neige_track_cat`.
    pub(crate) fn spelling(&self) -> String {
        let (object, action) = self.words();
        format!("{object} {action}")
    }
}

fn tool_words(tool: &str) -> Option<(&str, &str)> {
    tool.strip_prefix("neige_")?.split_once('_')
}

/// The command that serves `tool`, if any.
pub(crate) fn command_for_tool(tool: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.tool == tool)
}

/// `neige track cat` for a CLI-covered tool, `None` for every other tool.
pub(crate) fn cli_spelling(tool: &str) -> Option<String> {
    command_for_tool(tool).map(|command| format!("neige {}", command.spelling()))
}

/// The objects in table order, then the CLI-only `tool` meta object.
pub(crate) fn objects() -> Vec<&'static str> {
    let mut objects: Vec<&'static str> = Vec::new();
    for command in COMMANDS {
        let (object, _) = command.words();
        if !objects.contains(&object) {
            objects.push(object);
        }
    }
    objects.push(super::catalog::COMMAND_NAME);
    objects
}

/// The actions of `object` as typed, in table order.
pub(crate) fn actions(object: &str) -> Vec<String> {
    if object == super::catalog::COMMAND_NAME {
        return super::catalog::ACTIONS
            .iter()
            .map(|action| action.to_string())
            .collect();
    }
    COMMANDS
        .iter()
        .map(Command::words)
        .filter(|(candidate, _)| *candidate == object)
        .map(|(_, action)| action.to_string())
        .collect()
}

/// A positional fills its tool key; every positional is also accepted as its `--<key>` option.
pub(crate) struct Positional {
    pub(crate) key: &'static str,
    pub(crate) required: bool,
}

pub(crate) struct Opt {
    /// `--<key>` with `_` written `-`; only a [`OptValue::View`] has its own spelling.
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
    /// One comma-separated value, sent as the array of its pieces verbatim.
    CommaList,
    /// Present means `true`; shapes only the kernel's text output and never reaches the tool.
    View,
}

/// A destructive command runs only with `--force`, or with the `unless` tool key set.
pub(crate) struct Confirm {
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

const fn pos(key: &'static str, required: bool) -> Positional {
    Positional { key, required }
}

pub(crate) const FORCE: &str = "--force";
pub(crate) const JSON: &str = "--json";

/// `--<key>`, with `_` written `-`.
pub(crate) fn option_flag(key: &str) -> String {
    format!("--{}", key.replace('_', "-"))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command {
        tool: track_file::TOOL_TRACK_LS,
        positionals: &[pos("path", false)],
        options: &[opt("-l", "long", OptValue::View, false)],
        confirm: None,
        render: Render::Ls {
            long: false,
            reports: false,
        },
    },
    Command {
        tool: track_file::TOOL_TRACK_CAT,
        positionals: &[pos("path", true)],
        options: &[
            opt("--blocks", "blocks", OptValue::CommaList, false),
            opt("--sections", "sections", OptValue::CommaList, false),
        ],
        confirm: None,
        render: Render::Content,
    },
    Command {
        tool: track_history::TOOL_TRACK_SHOW,
        positionals: &[pos("commit", true), pos("path", true)],
        options: &[],
        confirm: None,
        render: Render::Content,
    },
    Command {
        tool: track_history::TOOL_TRACK_DIFF,
        positionals: &[pos("from", true), pos("to", false), pos("path", false)],
        options: &[],
        confirm: None,
        render: Render::Diff,
    },
    Command {
        tool: track_history::TOOL_TRACK_LOG,
        positionals: &[pos("path", false)],
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
        tool: track_state::TOOL_TRACK_STATE,
        positionals: &[],
        options: &[],
        confirm: None,
        render: Render::State,
    },
    Command {
        tool: track_state::TOOL_TRACK_CLOSE,
        positionals: &[],
        options: &[opt("--message", "message", OptValue::Text, true)],
        confirm: None,
        render: Render::Raw,
    },
    Command {
        tool: area_reports_tool::TOOL_REPORT_FIND,
        positionals: &[pos("path", true)],
        options: &[
            opt("--name", "name", OptValue::Text, false),
            opt("--tag", "tag", OptValue::Text, false),
        ],
        confirm: None,
        render: Render::Find,
    },
    Command {
        tool: report_tag::TOOL_REPORT_TAG,
        positionals: &[pos("path", true)],
        options: &[
            opt("--add", "add", OptValue::TextList, false),
            opt("--remove", "remove", OptValue::TextList, false),
        ],
        confirm: None,
        render: Render::Tags,
    },
    Command {
        tool: emit::TOOL_TASK_DONE,
        positionals: &[],
        options: &[
            opt("--attempt-id", "attempt_id", OptValue::Text, true),
            opt("--result", "result", OptValue::JsonOrText, false),
            opt("--artifacts", "artifacts", OptValue::TextList, false),
        ],
        confirm: None,
        render: Render::Raw,
    },
    Command {
        tool: emit::TOOL_TASK_FAIL,
        positionals: &[],
        options: &[
            opt("--attempt-id", "attempt_id", OptValue::Text, true),
            opt("--reason", "reason", OptValue::Text, true),
        ],
        confirm: None,
        render: Render::Raw,
    },
    Command {
        tool: admin::TOOL_ADMIN_GC,
        positionals: &[],
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
            unless: Some("dry_run"),
            refusal: "admin gc is destructive (prunes VCS history + sweeps objects); re-run with --force to confirm",
        }),
        render: Render::Raw,
    },
    Command {
        tool: admin::TOOL_ADMIN_VACUUM,
        positionals: &[],
        options: &[],
        confirm: Some(Confirm {
            unless: None,
            refusal: "admin vacuum write-locks the DB and must run in a quiet maintenance window; re-run with --force to confirm",
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

/// A refusal before any tool runs; `command` (a tool name) selects the usage line shown with `--json`.
#[derive(Debug, PartialEq)]
pub(crate) struct Usage {
    pub(crate) message: String,
    pub(crate) json: bool,
    pub(crate) command: Option<&'static str>,
}

pub(crate) fn parse(argv: &[String]) -> Result<Parsed, Usage> {
    let mut json = false;
    let mut iter = argv.iter();
    let mut next_word = |json: &mut bool| loop {
        match iter.next().map(String::as_str) {
            Some(JSON) => *json = true,
            other => break other,
        }
    };
    let Some(object) = next_word(&mut json) else {
        return Err(usage(missing_command(), json, None));
    };
    if !objects().contains(&object) {
        return Err(usage(help::unknown_command_message(object), json, None));
    }
    let Some(action) = next_word(&mut json) else {
        return Err(usage(missing_action(object), json, None));
    };
    let Some(command) = COMMANDS.iter().find(|c| c.is(object, action)) else {
        return Err(usage(
            help::unknown_action_message(object, action),
            json,
            None,
        ));
    };
    let fail = |message: String, json: bool| usage(message, json, Some(command.tool));
    let cmd = command.spelling();

    let mut args = Map::new();
    let mut positionals = Vec::new();
    let mut forced = false;
    let mut views: Vec<&'static str> = Vec::new();
    while let Some(arg) = iter.next() {
        if arg == JSON {
            json = true;
            continue;
        }
        if command.confirm.is_some() && arg == FORCE {
            forced = true;
            continue;
        }
        if !arg.starts_with('-') {
            positionals.push(arg.clone());
            continue;
        }
        let (flag, key, value) = if let Some(opt) = command.options.iter().find(|o| o.flag == arg) {
            (opt.flag.to_string(), opt.key, &opt.value)
        } else if let Some(slot) = command
            .positionals
            .iter()
            .find(|p| option_flag(p.key) == *arg)
        {
            (option_flag(slot.key), slot.key, &OptValue::Text)
        } else {
            return Err(fail(unknown_option(command, arg), json));
        };
        if let OptValue::Flag = value {
            args.insert(key.into(), Value::Bool(true));
            continue;
        }
        if let OptValue::View = value {
            views.push(key);
            continue;
        }
        let Some(raw) = iter.next() else {
            return Err(fail(format!("{cmd} requires a value after {flag}"), json));
        };
        let value = match value {
            OptValue::Flag | OptValue::View => unreachable!("flags take no value"),
            OptValue::Text => Value::String(raw.clone()),
            OptValue::Integer { .. } => raw
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| fail(format!("{cmd} {flag} must be a non-negative integer"), json))?,
            OptValue::JsonOrText => {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
            }
            OptValue::CommaList => raw
                .split(',')
                .map(|piece| Value::String(piece.to_string()))
                .collect(),
            OptValue::TextList => {
                let list = args.entry(key).or_insert_with(|| Value::Array(Vec::new()));
                list.as_array_mut()
                    .expect("list options hold arrays")
                    .push(Value::String(raw.clone()));
                continue;
            }
        };
        if args.insert(key.into(), value).is_some() {
            return Err(fail(format!("{cmd} accepts {flag} once"), json));
        }
    }

    // A named `--<key>` claims its slot; positionals fill the unclaimed slots in order.
    let open: Vec<&Positional> = command
        .positionals
        .iter()
        .filter(|slot| !args.contains_key(slot.key))
        .collect();
    if let Some(extra) = positionals.get(open.len()) {
        return Err(fail(
            format!(
                "unexpected argument `{extra}`; usage: {}",
                help::usage_line(Some(command.tool))
            ),
            json,
        ));
    }
    for (slot, value) in open.iter().zip(&positionals) {
        args.insert(slot.key.into(), Value::String(value.clone()));
    }
    for slot in command.positionals {
        if slot.required && !args.contains_key(slot.key) {
            return Err(fail(format!("{cmd} requires <{}>", slot.key), json));
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

    let render = match command.render {
        Render::Ls { .. } => Render::Ls {
            long: views.contains(&"long"),
            reports: args
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| {
                    matches!(
                        area_reports::classify(&normalize_path(path)),
                        Some(Ok(AreaPath::Reports))
                    )
                }),
        },
        other => other,
    };
    Ok(Parsed {
        tool: command.tool,
        args: Value::Object(args),
        json,
        render,
    })
}

/// Every flag `command` accepts: its options, its positionals as options, then the global ones.
pub(crate) fn accepted_flags(command: &Command) -> Vec<String> {
    let mut flags: Vec<String> = command.options.iter().map(|o| o.flag.to_string()).collect();
    flags.extend(command.positionals.iter().map(|p| option_flag(p.key)));
    if command.confirm.is_some() {
        flags.push(FORCE.into());
    }
    flags.push(JSON.into());
    flags
}

fn unknown_option(command: &Command, arg: &str) -> String {
    format!(
        "unknown option `{arg}` for `neige {}`; expected one of: {}",
        command.spelling(),
        accepted_flags(command).join(", ")
    )
}

fn usage(message: String, json: bool, command: Option<&'static str>) -> Usage {
    Usage {
        message,
        json,
        command,
    }
}

fn missing_command() -> String {
    format!(
        "missing command; expected `neige <object> <action>` with an object of: {}",
        objects().join(", ")
    )
}

fn missing_action(object: &str) -> String {
    format!(
        "`neige {object}` needs an action: {}",
        actions(object).join(", ")
    )
}

#[cfg(test)]
mod tests;
