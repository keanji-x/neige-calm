//! `neige` help text (#1801, #2003). Served by the kernel like every other argv; the forwarder
//! answers only a lone `--version`. Each command's help is keyed by the tool it calls, and its
//! spelling `neige <object> <action>` is derived from that tool's name.

use std::fmt::Write as _;

use super::catalog;
use super::commands::{self, COMMANDS};
use crate::mcp_server::tools::{
    admin, area_reports, emit, report_tag, track_file, track_history, track_state,
};
use crate::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP;

#[derive(Clone, Copy, Debug)]
pub enum HelpRequest<'a> {
    Root,
    /// `neige help <object>` or `neige <object> --help`: the object's actions.
    Object(&'a str),
    /// `neige help <object> <action>` or `neige <object> <action> … --help`.
    Command(&'a str, &'a str),
}

/// The help of one command, keyed by its tool name (or [`catalog::COMMAND_NAME`] for the meta command).
struct CommandHelp {
    key: &'static str,
    summary: &'static str,
    text: &'static str,
}

macro_rules! command_help {
    ($key:expr, $summary:literal, $($line:literal),+ $(,)?) => {
        CommandHelp {
            key: $key,
            summary: $summary,
            text: concat!($summary, ".\n\n", $($line, "\n"),+),
        }
    };
}

const HELP: &[CommandHelp] = &[
    command_help!(
        track_file::TOOL_TRACK_LS,
        "List files and directories in the current track view",
        "Usage: neige track ls [path] [-l] [--json]",
        "",
        "Arguments:",
        "  [path]  View path to list (also --path)",
        "",
        "Planner: `area/reports/` lists the reports of every track in this area, own track",
        "included, newest report update first, one `<title>.md` per line. A title shared by",
        "several tracks lists as `<title>~<id prefix>.md`; `%`, `/`, `~`, `\\`, control characters",
        "and a leading `.` in a title are written as `%XX`. With -l: UPDATED_AT (the report's own",
        "update time, server local time), TAGS (comma-joined, `—` for none) and NAME. At most",
        "500 reports; narrow a larger area with `neige report find`.",
        "",
        "Options:",
        "  -l          Long listing: update times (and tags under area/reports/)",
        "      --json  Emit compact JSON output; area/reports/ gives",
        "              [{path, title, trackId, tags, updatedAt}]",
        "  -h, --help  Print help",
    ),
    command_help!(
        track_file::TOOL_TRACK_CAT,
        "Print one file from the current track view",
        "Usage: neige track cat <path> [--blocks <id,...> | --sections <heading,...>] [--json]",
        "",
        "Arguments:",
        "  <path>  View path to read (also --path)",
        "",
        "Planner: `area/reports/<name>.md`, a name `neige track ls` or `neige report find` printed,",
        "prints that report's latest Markdown body, exactly as its track's own `report.md`. A bare",
        "title shared by several tracks is refused with the `~<id prefix>` candidates.",
        "",
        "On a report path (`report.md`, `area/reports/<name>.md`), --blocks prints only those",
        "blocks, in document order, each after its `<!-- neige:<id> -->` marker line: the text",
        "neige.report.read gives for select.blocks. An unknown id is refused with the report's",
        "blocks listed as `<id>  <heading>` lines. Any other path refuses --blocks.",
        "--sections does the same for whole H1 sections, named by their heading text (the `# `",
        "line without `# `): the text neige.report.read gives for select.sections. An unknown",
        "section is refused with the report's H1 sections listed.",
        "`track cat` is a view; a report write needs `neige.report.read` first.",
        "",
        "Options:",
        "      --blocks <id,...>            Print only these blocks of a report (comma-separated ids)",
        "      --sections <heading,...>     Print only these H1 sections of a report",
        "      --json                       Emit errors as JSON",
        "  -h, --help                       Print help",
    ),
    command_help!(
        track_history::TOOL_TRACK_SHOW,
        "Print a file as it existed at a commit",
        "Usage: neige track show <commit> <path> [--json]",
        "",
        "Arguments:",
        "  <commit>  Commit to read, full hash or unique prefix (also --commit)",
        "  <path>    View path to read (also --path)",
        "",
        "Options:",
        "      --json  Emit errors as JSON",
        "  -h, --help  Print help",
    ),
    command_help!(
        track_history::TOOL_TRACK_DIFF,
        "Show track changes between commits",
        "Usage: neige track diff <from> [to] [path] [--json]",
        "",
        "Arguments:",
        "  <from>  Starting commit, full hash or unique prefix (also --from)",
        "  [to]    Ending commit, full hash or unique prefix (also --to)",
        "  [path]  Limit the diff to one view path (also --path)",
        "",
        "Options:",
        "      --json  Emit compact JSON output",
        "  -h, --help  Print help",
    ),
    command_help!(
        track_history::TOOL_TRACK_LOG,
        "Show track commits that changed files",
        "Usage: neige track log [path] [--limit <count>] [--include-empty] [--json]",
        "",
        "Arguments:",
        "  [path]  Limit history to one view path (also --path)",
        "",
        "Options:",
        "      --limit <count>  Maximum number of commits to show",
        "      --include-empty  Include commits without file changes",
        "      --json           Emit compact JSON output",
        "  -h, --help           Print help",
    ),
    command_help!(
        track_state::TOOL_TRACK_STATE,
        "Show the track's current state: closed_at, your card, report, tasks and live sessions",
        "Usage: neige track state [--json]",
        "",
        "Options:",
        "      --json  Emit compact JSON output",
        "  -h, --help  Print help",
    ),
    command_help!(
        track_state::TOOL_TRACK_CLOSE,
        "Close this track (Planner)",
        "Usage: neige track close --message <text> [--json]",
        "",
        "Options:",
        "      --message <text>  Why the track is closed",
        "      --json            Emit errors as JSON",
        "  -h, --help            Print help",
    ),
    command_help!(
        area_reports::TOOL_REPORT_FIND,
        "Search this area's reports by name glob and tag (Planner)",
        "Usage: neige report find area/reports/ [--name <glob>] [--tag <tag>] [--json]",
        "",
        "Arguments:",
        "  <path>  area/reports/, the only searchable directory (also --path)",
        "",
        "Prints one `area/reports/<name>.md` path per line, ready for `neige track cat`, newest",
        "report update first; no match prints nothing and succeeds. --name globs the listed file",
        "name (`*` any run, `?` one character, everything else literal); --tag matches one tag",
        "exactly; both given means AND. At most 500 matches; more is refused, so narrow the search.",
        "",
        "Options:",
        "      --name <glob>  Match the file name, e.g. '*认证*'",
        "      --tag <tag>    Match one tag exactly",
        "      --json         Emit [{path, title, trackId, tags, updatedAt}]",
        "  -h, --help         Print help",
    ),
    command_help!(
        report_tag::TOOL_REPORT_TAG,
        "Show or change the tags of this track's report",
        "Usage: neige report tag <path> [--add <tag>]... [--remove <tag>]... [--json]",
        "",
        "Arguments:",
        "  <path>  report.md, the only report that takes tags here (also --path)",
        "",
        "Prints the current tags space-joined on one line (an empty line when there are none).",
        "Adds apply before removes; adding a present tag or removing an absent one is a no-op.",
        "Only the Planner changes tags; a worker may list them.",
        "",
        "Options:",
        "      --add <tag>     Add a tag; may be repeated",
        "      --remove <tag>  Remove a tag; may be repeated",
        "      --json          Emit compact JSON output",
        "  -h, --help          Print help",
    ),
    command_help!(
        emit::TOOL_TASK_REPORT_SUCCESS,
        "Report the successful outcome of a worker execution",
        "Usage: neige task report-success --attempt-id <id> [--result <json-or-text>] [--artifacts <path>]... [--json]",
        "",
        "Returns report_received; delivery, verification and Planner acceptance are separate.",
        "Stop changing the workspace and end your turn after reporting.",
        "",
        "Options:",
        "      --attempt-id <id>        The task attempt_id handed to you",
        "      --result <json-or-text>  Optional result as JSON or plain text",
        "      --artifacts <path>       Attach an artifact path; may be repeated",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        emit::TOOL_TASK_REPORT_FAILURE,
        "Report the failed outcome of a worker execution",
        "Usage: neige task report-failure --attempt-id <id> --reason <text> [--json]",
        "",
        "Returns report_received; failed-work delivery and Track closure are separate.",
        "Stop changing the workspace and end your turn after reporting.",
        "",
        "Options:",
        "      --attempt-id <id>        The task attempt_id handed to you",
        "      --reason <text>          Failure reason",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        admin::TOOL_ADMIN_GC,
        "Prune track history and sweep unreferenced objects",
        "Usage: neige admin gc --track-id <id> [--keep <count>] [--dry-run] [--force] [--json]",
        "",
        "Options:",
        "      --track-id <id>  Track to prune",
        "      --keep <count>   Number of recent commits to keep [default: {default_keep}]",
        "      --dry-run        Report what would be pruned without changing data",
        "      --force          Confirm destructive pruning (required without --dry-run)",
        "      --json           Emit errors as JSON",
        "  -h, --help           Print help",
    ),
    command_help!(
        admin::TOOL_ADMIN_VACUUM,
        "Reclaim free space in the SQLite database",
        "Usage: neige admin vacuum --force [--json]",
        "",
        "Options:",
        "      --force  Confirm the full-database maintenance lock",
        "      --json   Emit errors as JSON",
        "  -h, --help   Print help",
    ),
    command_help!(
        catalog::COMMAND_NAME,
        "List and describe the tools of this session",
        "Usage: neige tool list (--prefix PREFIX | --all) [--after NAME] [--json]",
        "       neige tool describe --name NAME [--json]",
        "",
        "List rows are {name, cli, listed}: this session's tools/list set plus every tool a",
        "`neige` command calls. cli is that command or null; listed says whether tools/list shows",
        "the tool to this session. Names are literal MCP names, so --prefix is no regex.",
        "At most 20 rows per page with next_cursor; pass it as --after for the next page.",
        "Describe prints one tool's MCP declaration plus cli and listed.",
        "A client calls a neige.<object>.<action> tool as neige_<object>_<action>.",
        "Listing is not a grant; the tool's role gate decides.",
        "",
        "Options:",
        "      --json  Emit compact JSON output",
        "  -h, --help  Print help",
    ),
];

const HELP_SUMMARY: &str = "Print global, object or command help";

/// `track cat` for a command's tool key, `tool list|describe` for the meta command.
fn spelling(key: &str) -> String {
    match commands::command_for_tool(key) {
        Some(command) => command.spelling(),
        None => format!("{key} {}", catalog::ACTIONS.join("|")),
    }
}

fn help_for(key: &str) -> Option<&'static CommandHelp> {
    HELP.iter().find(|help| help.key == key)
}

pub fn request(args: &[String]) -> Option<HelpRequest<'_>> {
    let is_help = |arg: &str| matches!(arg, "--help" | "-h");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != commands::JSON)
        .collect();
    match words.as_slice() {
        [] => None,
        [first, ..] if is_help(first) => Some(HelpRequest::Root),
        ["help", rest @ ..] => match rest {
            [] => Some(HelpRequest::Root),
            [first, ..] if is_help(first) => Some(HelpRequest::Root),
            [object] => Some(HelpRequest::Object(object)),
            [object, second, ..] if is_help(second) => Some(HelpRequest::Object(object)),
            [object, action, ..] => Some(HelpRequest::Command(object, action)),
        },
        [object, rest @ ..] if rest.iter().any(|arg| is_help(arg)) => match rest.first() {
            Some(action) if !action.starts_with('-') => Some(HelpRequest::Command(object, action)),
            _ => Some(HelpRequest::Object(object)),
        },
        _ => None,
    }
}

pub fn render(request: HelpRequest<'_>) -> Option<String> {
    let text = match request {
        HelpRequest::Root => return Some(root_help()),
        HelpRequest::Object(object) if object == catalog::COMMAND_NAME => help_for(object)?.text,
        HelpRequest::Object(object) => return object_help(object),
        HelpRequest::Command(object, action) if object == catalog::COMMAND_NAME => {
            catalog::ACTIONS.contains(&action).then_some(())?;
            help_for(object)?.text
        }
        HelpRequest::Command(object, action) => {
            let command = COMMANDS.iter().find(|c| c.is(object, action))?;
            help_for(command.tool)?.text
        }
    };
    Some(text.replace(
        "{default_keep}",
        &DEFAULT_TRACK_HISTORY_PRUNE_KEEP.to_string(),
    ))
}

fn object_help(object: &str) -> Option<String> {
    let commands: Vec<_> = COMMANDS
        .iter()
        .filter(|command| command.words().0 == object)
        .collect();
    if commands.is_empty() {
        return None;
    }
    let mut output =
        format!("neige {object}\n\nUsage: neige {object} <action> [options]\n\nActions:\n");
    let width = commands
        .iter()
        .map(|command| command.words().1.len())
        .max()
        .unwrap_or(0)
        .max(10);
    for command in commands {
        let summary = help_for(command.tool).map_or("", |help| help.summary);
        writeln!(output, "  {:<width$} {summary}", command.words().1)
            .expect("writing help to a String cannot fail");
    }
    writeln!(
        output,
        "\nRun `neige help {object} <action>` for an action's help."
    )
    .expect("writing help to a String cannot fail");
    Some(output)
}

/// Every served spelling, in help order, ending with `help`.
pub fn available_commands() -> String {
    HELP.iter()
        .map(|help| spelling(help.key))
        .chain(std::iter::once("help".to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `neige <word>` names no object (an old one-word spelling included).
pub fn unknown_command_message(word: &str) -> String {
    format!(
        "unknown command `{word}`; a command is `neige <object> <action>`\n\nObjects: {}\nRun `neige --help` for usage.",
        commands::objects().join(", ")
    )
}

pub fn unknown_action_message(object: &str, action: &str) -> String {
    format!(
        "unknown action `{action}` for `neige {object}`; expected one of: {}",
        commands::actions(object).join(", ")
    )
}

/// Why `request` has no help: the unknown object or action, with the valid choices.
pub fn unknown_help_message(request: HelpRequest<'_>) -> String {
    match request {
        HelpRequest::Object(object) | HelpRequest::Command(object, _)
            if !commands::objects().contains(&object) =>
        {
            unknown_command_message(object)
        }
        HelpRequest::Command(object, action) => unknown_action_message(object, action),
        HelpRequest::Root | HelpRequest::Object(_) => {
            unreachable!("root help and a known object's help always render")
        }
    }
}

fn root_help() -> String {
    let mut output = String::from(concat!(
        "neige\n\n",
        "Read track views, inspect history, and report worker tasks.\n\n",
        "Usage: neige [--json] <object> <action> [options]\n",
        "       neige help [<object> [<action>]]\n\n",
        "Commands:\n",
    ));
    for help in HELP {
        writeln!(output, "  {:<20} {}", spelling(help.key), help.summary)
            .expect("writing help to a String cannot fail");
    }
    writeln!(output, "  {:<20} {HELP_SUMMARY}", "help")
        .expect("writing help to a String cannot fail");
    output.push_str(concat!(
        "\nEach command calls the tool neige.<object>.<action>; each option is --<input key>, with\n",
        "`_` written `-`, and a positional argument is also accepted as its --<key> option.\n",
        "\nOptions:\n",
        "      --json     Use JSON output where supported; otherwise emit errors as JSON\n",
        "      --force    Confirm a destructive command\n",
        "      --version  (only argument): print the forwarder version\n",
        "  -h, --help     Print help\n\n",
        "Run `neige help <object> <action>` for command-specific help.\n",
    ));
    output
}

/// The synopsis of the help keyed `command` (a tool name, or the meta command), or of the root
/// help without one; `--json` usage errors carry it.
pub fn usage_line(command: Option<&str>) -> String {
    let text = match command {
        Some(key) => help_for(key)
            .expect("usage lines are asked only for served commands")
            .text
            .to_string(),
        None => root_help(),
    };
    text.lines()
        .find_map(|line| line.strip_prefix("Usage: "))
        .expect("every help text has a Usage line")
        .to_string()
}
