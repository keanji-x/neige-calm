use std::fmt::Write as _;

#[derive(Clone, Copy, Debug)]
pub(crate) enum HelpRequest<'a> {
    Root,
    Command(&'a str),
}

impl<'a> HelpRequest<'a> {
    pub(crate) fn command(self) -> Option<&'a str> {
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
    ($name:literal, $summary:literal, $($line:literal),+ $(,)?) => {
        CommandHelp {
            name: $name,
            summary: $summary,
            text: concat!($summary, ".\n\n", $($line, "\n"),+),
        }
    };
}

const COMMANDS: &[CommandHelp] = &[
    command_help!(
        "ls",
        "List files and directories in the current track view",
        "Usage: neige ls [path] [--json]",
        "",
        "Arguments:",
        "  [path]  View path to list [default: /]",
        "",
        "Options:",
        "      --json  Emit compact JSON output",
        "  -h, --help  Print help",
    ),
    command_help!(
        "cat",
        "Print one file from the current track view",
        "Usage: neige cat <path> [--json]",
        "",
        "Arguments:",
        "  <path>  View path to read",
        "",
        "Options:",
        "      --json  Emit errors as JSON",
        "  -h, --help  Print help",
    ),
    command_help!(
        "state",
        "Show track and card metadata",
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
        "  [to]    Ending commit (full hash or unique prefix) [default: current track commit]",
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
        "task-completed",
        "Report successful completion of a worker task",
        "Usage: neige task-completed --idempotency-key <key> [--result <json-or-text>] [--artifact <path>]... [--json]",
        "",
        "Options:",
        "      --idempotency-key <key>  Idempotency key supplied with the task",
        "      --result <json-or-text>  Optional result as JSON or plain text",
        "      --artifact <path>        Attach an artifact path; may be repeated",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        "task-failed",
        "Report failure of a worker task",
        "Usage: neige task-failed --idempotency-key <key> --reason <text> [--json]",
        "",
        "Options:",
        "      --idempotency-key <key>  Idempotency key supplied with the task",
        "      --reason <text>          Failure reason",
        "      --json                   Emit errors as JSON",
        "  -h, --help                   Print help",
    ),
    command_help!(
        "track-gc",
        "Prune track history and sweep unreferenced objects",
        "Usage: neige track-gc --track-id <id> [--keep <count>] [--dry-run] [--force] [--json]",
        "",
        "Options:",
        "      --track-id <id>  Track to prune",
        "      --keep <count>   Number of recent commits to keep [default: 50]",
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

pub(crate) fn request(args: &[String]) -> Option<HelpRequest<'_>> {
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

pub(crate) fn render(request: HelpRequest<'_>) -> Option<String> {
    match request {
        HelpRequest::Root => Some(root_help()),
        HelpRequest::Command(command) => COMMANDS
            .iter()
            .find(|candidate| candidate.name == command)
            .map(|command| command.text.to_owned()),
    }
}

pub(crate) fn available_commands() -> String {
    COMMANDS
        .iter()
        .map(|command| command.name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn unknown_command_message(command: &str) -> String {
    format!(
        "unknown command `{command}`\n\nAvailable commands: {}\nRun `neige --help` for usage.",
        available_commands()
    )
}

fn root_help() -> String {
    let mut output = format!(
        concat!(
            "neige {}\n\n",
            "Read track views, inspect history, and report worker tasks.\n\n",
            "Usage: neige [--json] <command> [options]\n",
            "       neige help [command]\n\n",
            "Commands:\n",
        ),
        env!("CARGO_PKG_VERSION")
    );
    for command in COMMANDS {
        writeln!(output, "  {:<16} {}", command.name, command.summary)
            .expect("writing help to a String cannot fail");
    }
    output.push_str(concat!(
        "\nOptions:\n",
        "      --json     Use JSON output where supported; otherwise emit errors as JSON\n",
        "      --version  Print version\n",
        "  -h, --help     Print help\n\n",
        "Run `neige help <command>` for command-specific help.\n",
    ));
    output
}
