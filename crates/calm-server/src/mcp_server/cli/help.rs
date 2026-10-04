//! `neige` help text, moved verbatim from the former fat client (#1801). Served by the kernel like
//! every other argv; the forwarder answers only a lone `--version`.

use std::fmt::Write as _;

use crate::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP;

#[derive(Clone, Copy, Debug)]
pub enum HelpRequest<'a> {
    Root,
    Command(&'a str),
}

impl<'a> HelpRequest<'a> {
    pub fn command(self) -> Option<&'a str> {
        match self {
            Self::Root => None,
            Self::Command(command) => Some(command),
        }
    }
}

struct CommandHelp {
    name: &'static str,
    summary: &'static str,
    text: &'static str,
}

macro_rules! command_help {
    ($name:expr, $summary:literal, $($line:literal),+ $(,)?) => {
        CommandHelp {
            name: $name,
            summary: $summary,
            text: concat!($summary, ".\n\n", $($line, "\n"),+),
        }
    };
}

const COMMANDS: &[CommandHelp] = &[
    command_help!(
        super::catalog::COMMAND_NAME,
        "Find tools visible to the current active session",
        "Usage: neige tools names (--prefix PREFIX | --all) [--after NAME] [--json]",
        "       neige tools describe --name NAME [--json]",
        "Names are literal MCP wire names, scoped to your role and Track; no regex matching.",
        "Client runtime callable identifiers can differ; use the client's exact-name loader.",
        "Names returns at most 20 tools and next_cursor; pass it as --after for the next page.",
        "Describe returns one visible tool's MCP declaration; lookup never grants invocation rights.",
    ),
    command_help!(
        "ls",
        "List files and directories in the current track view",
        "Usage: neige ls [path] [-l] [--json]",
        "",
        "Arguments:",
        "  [path]  View path to list",
        "",
        "Planner: `area/reports/` lists the reports of every track in this area, own track",
        "included, newest report update first, one `<title>.md` per line. A title shared by",
        "several tracks lists as `<title>~<id prefix>.md`; `%`, `/`, `~`, `\\`, control characters",
        "and a leading `.` in a title are written as `%XX`. With -l: UPDATED_AT (the report's own",
        "update time, server local time), TAGS (comma-joined, `—` for none) and NAME. At most",
        "500 reports; narrow a larger area with `neige find`.",
        "",
        "Options:",
        "  -l          Long listing: update times (and tags under area/reports/)",
        "      --json  Emit compact JSON output; area/reports/ gives",
        "              [{path, title, trackId, tags, updatedAt}]",
        "  -h, --help  Print help",
    ),
    command_help!(
        "cat",
        "Print one file from the current track view",
        "Usage: neige cat <path> [--blocks <id,...> | --sections <heading,...>] [--json]",
        "",
        "Arguments:",
        "  <path>  View path to read",
        "",
        "Planner: `area/reports/<name>.md`, a name `neige ls` or `neige find` printed, prints that",
        "report's latest Markdown body, exactly as its track's own `report.md`. A bare title shared",
        "by several tracks is refused with the `~<id prefix>` candidates.",
        "",
        "On a report path (`report.md`, `area/reports/<name>.md`), --blocks prints only those",
        "blocks, in document order, each after its `<!-- neige:<id> -->` marker line: the text",
        "neige.report.read gives for select.blocks. An unknown id is refused with the report's",
        "blocks listed as `<id>  <heading>` lines. Any other path refuses --blocks.",
        "--sections does the same for whole H1 sections, named by their heading text (the `# `",
        "line without `# `): the text neige.report.read gives for select.sections. An unknown",
        "section is refused with the report's H1 sections listed. cat is a view: a report write",
        "needs a neige.report.read first.",
        "",
        "Options:",
        "      --blocks <id,...>            Print only these blocks of a report (comma-separated ids)",
        "      --sections <heading,...>     Print only these H1 sections of a report",
        "      --json                       Emit errors as JSON",
        "  -h, --help                       Print help",
    ),
    command_help!(
        "find",
        "Search this area's reports by name glob and tag (Planner)",
        "Usage: neige find area/reports/ [-name <glob>] [-tag <tag>] [--json]",
        "",
        "Arguments:",
        "  <path>  area/reports/ (the only searchable directory)",
        "",
        "Prints one `area/reports/<name>.md` path per line, ready for `neige cat`, newest report",
        "update first; no match prints nothing and succeeds. -name globs the listed file name",
        "(`*` any run, `?` one character, everything else literal); -tag matches one tag exactly",
        "(a Neige extension, not a system find option); both given means AND. At most 500 matches;",
        "more is refused, so narrow the search.",
        "",
        "Options:",
        "      -name <glob>  Match the file name, e.g. '*认证*'",
        "      -tag <tag>    Match one tag exactly",
        "      --json        Emit [{path, title, trackId, tags, updatedAt}]",
        "  -h, --help        Print help",
    ),
    command_help!(
        "state",
        "Show the track's current state: closed_at, your card, report, tasks and live sessions",
        "Usage: neige state [--json]",
        "",
        "Options:",
        "      --json  Emit compact JSON output",
        "  -h, --help  Print help",
    ),
    command_help!(
        "diff",
        "Show track changes between commits",
        "Usage: neige diff <from> [to] [path] [--to <commit>] [--path <path>] [--json]",
        "",
        "Arguments:",
        "  <from>  Starting commit (full hash or unique prefix)",
        "  [to]    Ending commit (full hash or unique prefix)",
        "  [path]  Limit the diff to one view path",
        "",
        "Options:",
        "      --to <commit>  Ending commit (alternative to positional [to])",
        "      --path <path>  View path (alternative to positional [path])",
        "      --json         Emit compact JSON output",
        "  -h, --help         Print help",
    ),
    command_help!(
        "cat-at",
        "Print a file as it existed at a commit",
        "Usage: neige cat-at <commit> <path> [--json]",
        "",
        "Arguments:",
        "  <commit>  Commit to read (full hash or unique prefix)",
        "  <path>    View path to read",
        "",
        "Options:",
        "      --json  Emit errors as JSON",
        "  -h, --help  Print help",
    ),
    command_help!(
        "log",
        "Show track commits that changed files",
        "Usage: neige log [path] [--limit <count>] [--include-empty] [--json]",
        "",
        "Arguments:",
        "  [path]  Limit history to one view path",
        "",
        "Options:",
        "      --limit <count>  Maximum number of commits to show",
        "      --include-empty   Include commits without file changes",
        "      --json            Emit compact JSON output",
        "  -h, --help            Print help",
    ),
    command_help!(
        "tag",
        "Show or change the tags of this track's report",
        "Usage: neige tag <path> [--add <tag>]... [--remove <tag>]... [--json]",
        "",
        "Arguments:",
        "  <path>  report.md (only this track's own report takes tags)",
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
        "task-completed",
        "Report successful completion of a worker task",
        "Usage: neige task-completed --attempt-id <id> [--result <json-or-text>] [--artifact <path>]... [--json]",
        "",
        "Options:",
        "      --attempt-id <id>        The task attempt_id handed to you",
        "      --result <json-or-text>  Optional result as JSON or plain text",
        "      --artifact <path>        Attach an artifact path; may be repeated",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        "task-failed",
        "Report failure of a worker task",
        "Usage: neige task-failed --attempt-id <id> --reason <text> [--json]",
        "",
        "Options:",
        "      --attempt-id <id>        The task attempt_id handed to you",
        "      --reason <text>          Failure reason",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        "track-close",
        "Close this track (Planner)",
        "Usage: neige track-close --message <text> [--json]",
        "",
        "Options:",
        "      --message <text>  Why the track is closed",
        "      --json            Emit errors as JSON",
        "  -h, --help            Print help",
    ),
    command_help!(
        "track-gc",
        "Prune track history and sweep unreferenced objects",
        "Usage: neige track-gc --track-id <id> [--keep <count>] [--dry-run] [--force] [--json]",
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
        "vacuum",
        "Reclaim free space in the SQLite database",
        "Usage: neige vacuum --force [--json]",
        "",
        "Options:",
        "      --force  Confirm the full-database maintenance lock",
        "      --json   Emit errors as JSON",
        "  -h, --help   Print help",
    ),
    command_help!(
        "help",
        "Print global or command-specific help",
        "Usage: neige help [command]",
        "",
        "Arguments:",
        "  [command]  Command whose help should be printed",
        "",
        "Options:",
        "  -h, --help  Print help",
    ),
];

pub fn request(args: &[String]) -> Option<HelpRequest<'_>> {
    let mut args = args.iter().filter(|arg| arg.as_str() != "--json");
    let first = args.next()?.as_str();

    match first {
        "--help" | "-h" => Some(HelpRequest::Root),
        "help" => match args.next().map(String::as_str) {
            None | Some("--help" | "-h") => Some(HelpRequest::Root),
            Some(command) => Some(HelpRequest::Command(command)),
        },
        command if args.any(|arg| matches!(arg.as_str(), "--help" | "-h")) => {
            Some(HelpRequest::Command(command))
        }
        _ => None,
    }
}

pub fn render(request: HelpRequest<'_>) -> Option<String> {
    match request {
        HelpRequest::Root => Some(root_help()),
        HelpRequest::Command(command) => COMMANDS
            .iter()
            .find(|candidate| candidate.name == command)
            .map(|command| {
                command.text.replace(
                    "{default_keep}",
                    &DEFAULT_TRACK_HISTORY_PRUNE_KEEP.to_string(),
                )
            }),
    }
}

pub fn available_commands() -> String {
    COMMANDS
        .iter()
        .map(|command| command.name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn unknown_command_message(command: &str) -> String {
    format!(
        "unknown command `{command}`\n\nAvailable commands: {}\nRun `neige --help` for usage.",
        available_commands()
    )
}

fn root_help() -> String {
    let mut output = String::from(concat!(
        "neige\n\n",
        "Read track views, inspect history, and report worker tasks.\n\n",
        "Usage: neige [--json] <command> [options]\n",
        "       neige help [command]\n\n",
        "Commands:\n",
    ));
    for command in COMMANDS {
        writeln!(output, "  {:<16} {}", command.name, command.summary)
            .expect("writing help to a String cannot fail");
    }
    output.push_str(concat!(
        "\nOptions:\n",
        "      --json     Use JSON output where supported; otherwise emit errors as JSON\n",
        "      --version  (only argument): print the forwarder version\n",
        "  -h, --help     Print help\n\n",
        "Run `neige help <command>` for command-specific help.\n",
    ));
    output
}

/// The synopsis of `command`'s help, or of the root help without one; `--json` usage errors carry it.
pub fn usage_line(command: Option<&str>) -> String {
    let text = match command {
        Some(command) => render(HelpRequest::Command(command)),
        None => Some(root_help()),
    }
    .expect("usage lines are asked only for served commands");
    text.lines()
        .find_map(|line| line.strip_prefix("Usage: "))
        .expect("every help text has a Usage line")
        .to_string()
}
