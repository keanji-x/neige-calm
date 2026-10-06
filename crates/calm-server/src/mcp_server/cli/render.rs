//! Text and `--json` rendering of one tool's `structuredContent`, moved from the former fat client
//! (#1801). Text rejects missing required fields. State JSON checks only the top-level object
//! and passes it through compactly, without validating task fields.

use std::borrow::Cow;

use calm_types::task_execution::{TaskAccess, TaskStart};
use serde_json::{Value, json};

use crate::model::TaskStatus;
use crate::session_projection_repo::WorkerSessionState;
use crate::track_vcs::DiffStatus;

mod listing;
mod mail;

/// How one command prints its tool result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Render {
    /// `track ls`: `long` is `-l`; `reports` when the path is the `area/reports/` directory (#1838).
    Ls {
        long: bool,
        reports: bool,
    },
    /// `report find`: one `area/reports/` path per line.
    Find,
    /// `track cat` and `track show`: the view content; `--json` changes only the error format.
    Content,
    Status,
    Diff,
    Log,
    /// `report tag`: the current tags space-joined on one line.
    Tags,
    /// `mail ls`: one mail per line (#2130).
    MailLs,
    /// `mail cat`: one mail and the hop a send from this turn would get (#2130).
    MailCat,
    /// Maintenance and task reports: the result as compact JSON.
    Raw,
}

#[derive(Debug, PartialEq)]
pub struct RenderError {
    pub message: String,
    pub detail: Value,
}

/// `tool` names the result's producer in error messages.
pub fn render(
    render: Render,
    tool: &str,
    json: bool,
    value: &Value,
) -> Result<String, RenderError> {
    match render {
        Render::Ls { long, reports } => listing::ls(tool, json, long, reports, value),
        Render::Find => listing::find(tool, json, value),
        Render::Content => content(tool, value),
        Render::Status => state(tool, json, value),
        Render::Diff if json => Ok(compact(value)),
        Render::Diff => diff(tool, value),
        Render::Log if json => Ok(compact(value)),
        Render::Log => log(tool, value),
        Render::Tags if json => Ok(compact(value)),
        Render::Tags => tags(tool, value),
        Render::MailLs => mail::ls(tool, json, value),
        Render::MailCat => mail::cat(tool, json, value),
        Render::Raw => Ok(compact(value)),
    }
}

fn compact(value: &Value) -> String {
    format!("{value}\n")
}

fn shape(message: String, tool: &str, key: &str, value: &Value) -> RenderError {
    RenderError {
        message,
        detail: json!({ "kind": "shape", "tool": tool, key: value }),
    }
}

fn required_str<'a>(
    value: &'a Value,
    field: &str,
    tool: &str,
    what: &str,
) -> Result<&'a str, RenderError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        shape(
            format!("{tool} {what} missing string {field}"),
            tool,
            what,
            value,
        )
    })
}

/// `content_type` is optional: without one, or with JSON that does not parse, the content prints as is.
fn content(tool: &str, value: &Value) -> Result<String, RenderError> {
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            shape(
                format!("{tool} returned content without a string content field"),
                tool,
                "value",
                value,
            )
        })?;
    if value.get("content_type").and_then(Value::as_str) == Some("application/json")
        && let Ok(parsed) = serde_json::from_str::<Value>(content)
    {
        return Ok(format!("{:#}\n", parsed));
    }
    Ok(content.to_string())
}

/// The label column of the `state` text; every fact line starts with one of these.
const STATE_LABEL_WIDTH: usize = "closed_at".len();

/// One fact per line, each fact once: an agent greps `^closed_at` and gets exactly this track's.
fn state(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    if !value.is_object() {
        return Err(shape(
            format!("{tool} returned non-object structuredContent"),
            tool,
            "value",
            value,
        ));
    }
    if json {
        return Ok(compact(value));
    }
    let track = required_object(value, "track", tool)?;
    let track_id = required_str(track, "id", tool, "track")?;
    let title = required_str(track, "title", tool, "track")?;
    let closed_at = match track.get("closed_at") {
        Some(Value::Null) => "-".to_string(),
        Some(Value::String(at)) if chrono::DateTime::parse_from_rfc3339(at).is_ok() => at.clone(),
        Some(Value::String(_)) => {
            return Err(shape(
                format!("{tool} track closed_at is not an RFC 3339 time"),
                tool,
                "track",
                track,
            ));
        }
        _ => {
            return Err(shape(
                format!("{tool} track missing string-or-null closed_at"),
                tool,
                "track",
                track,
            ));
        }
    };
    let caller = required_str(value, "caller_card_id", tool, "value")?;
    let read_required = value
        .get("report_startup_read_required")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            shape(
                format!("{tool} value missing bool report_startup_read_required"),
                tool,
                "value",
                value,
            )
        })?;
    let tasks = state_tasks(tool, required_array(value, "tasks", tool)?)?;
    let cards = state_cards(tool, required_array(value, "cards", tool)?)?;

    let you = cards.iter().find(|card| card.id == caller).ok_or_else(|| {
        shape(
            format!("{tool} caller card {caller} is not among the track's cards"),
            tool,
            "value",
            value,
        )
    })?;
    let report = if !cards.iter().any(|card| card.kind == "track-report") {
        "none"
    } else if read_required {
        "has content"
    } else {
        "empty skeleton"
    };
    let title = if title.trim().is_empty() {
        "(untitled)"
    } else {
        title
    };

    let mut out = String::new();
    // Every fact line is written here, escaped, so no free-text value can forge a second line.
    let mut fact = |label: &str, text: &str| {
        let text = escape_control(text);
        out.push_str(&format!("{label:<STATE_LABEL_WIDTH$}  {text}\n"));
    };
    fact("track", track_id);
    fact("title", title);
    fact("closed_at", &closed_at);
    fact("you", &format!("{} {}", you.id, you.role));
    fact("report", report);
    if tasks.is_empty() {
        fact("tasks", "-");
    }
    for (index, task) in tasks.iter().enumerate() {
        let suffix = match task.access {
            TaskAccess::ReadOnly => " read_only",
            TaskAccess::ReadWrite => "",
        };
        let line = format!(
            "{} {}{suffix} start={}",
            task.key,
            task.status.wire_label(),
            task.start.as_str()
        );
        fact(if index == 0 { "tasks" } else { "" }, &line);
    }
    let live: Vec<&StateCard<'_>> = cards.iter().filter(|card| card.live).collect();
    for (index, line) in card_lines(&live, caller).iter().enumerate() {
        fact(if index == 0 { "sessions" } else { "" }, line);
    }
    Ok(out)
}

/// One current task execution, a `tasks` line `<key> <status>[ read_only] start=<start>`.
struct StateTask<'a> {
    key: &'a str,
    status: TaskStatus,
    access: TaskAccess,
    start: TaskStart,
}

fn state_tasks<'a>(tool: &str, tasks: &'a [Value]) -> Result<Vec<StateTask<'a>>, RenderError> {
    tasks
        .iter()
        .map(|task| {
            let status = required_str(task, "status", tool, "task")?;
            let access = required_str(task, "access", tool, "task")?;
            let start = required_str(task, "start", tool, "task")?;
            Ok(StateTask {
                key: required_str(task, "key", tool, "task")?,
                status: serde_json::from_value(Value::from(status)).map_err(|_| {
                    shape(
                        format!("{tool} task has unknown status {status:?}"),
                        tool,
                        "task",
                        task,
                    )
                })?,
                access: serde_json::from_value(Value::from(access)).map_err(|_| {
                    shape(
                        format!("{tool} task has unknown access {access:?}"),
                        tool,
                        "task",
                        task,
                    )
                })?,
                start: serde_json::from_value(Value::from(start)).map_err(|_| {
                    shape(
                        format!("{tool} task has unknown start {start:?}"),
                        tool,
                        "task",
                        task,
                    )
                })?,
            })
        })
        .collect()
}

struct StateCard<'a> {
    id: &'a str,
    role: &'a str,
    kind: &'a str,
    /// The worker session's status, `-` without a runtime row.
    status: &'a str,
    /// The runtime is an active worker session.
    live: bool,
}

fn state_cards<'a>(tool: &str, cards: &'a [Value]) -> Result<Vec<StateCard<'a>>, RenderError> {
    cards
        .iter()
        .map(|card| {
            let id = required_str(card, "id", tool, "card")?;
            let (status, live) = match card.get("runtime") {
                Some(Value::Null) => ("-", false),
                Some(runtime @ Value::Object(_)) => {
                    let status = required_str(runtime, "status", tool, "runtime")?;
                    let state = serde_json::from_value::<WorkerSessionState>(Value::from(status))
                        .map_err(|_| {
                        shape(
                            format!("{tool} runtime has unknown status {status:?}"),
                            tool,
                            "runtime",
                            runtime,
                        )
                    })?;
                    (status, state.is_active_authority())
                }
                _ => {
                    return Err(shape(
                        format!("{tool} card missing object-or-null runtime"),
                        tool,
                        "card",
                        card,
                    ));
                }
            };
            Ok(StateCard {
                id,
                role: required_str(card, "role", tool, "card")?,
                kind: required_str(card, "kind", tool, "card")?,
                status,
                live,
            })
        })
        .collect()
}

/// `id  role  kind  session <status>` (running means an open session, not a running task), columns (escaped first) padded to the widest value; task
/// status is the `tasks` block's. The caller's own row shows `(you)` for its session, which is
/// always mid-turn.
fn card_lines(cards: &[&StateCard<'_>], caller: &str) -> Vec<String> {
    let rows: Vec<[Cow<'_, str>; 4]> = cards
        .iter()
        .map(|card| {
            let session = if card.id == caller {
                Cow::Borrowed("(you)")
            } else {
                let status = if card.status == "running" {
                    "open"
                } else {
                    card.status
                };
                Cow::Owned(format!("session {status}"))
            };
            let [id, role, kind] = [card.id, card.role, card.kind].map(escape_control);
            [id, role, kind, session]
        })
        .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (id_w, role_w, kind_w) = (width(0), width(1), width(2));
    rows.iter()
        .map(|[id, role, kind, session]| {
            format!("{id:<id_w$}  {role:<role_w$}  {kind:<kind_w$}  {session}")
        })
        .collect()
}

/// Control characters as escapes (`\n`, `\r`, `\t`, else `\u{..}`); ordinary text, including
/// non-ASCII, is unchanged. The output holds no control character, so escaping twice is a no-op.
fn escape_control(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn required_object<'a>(
    value: &'a Value,
    field: &str,
    tool: &str,
) -> Result<&'a Value, RenderError> {
    value.get(field).filter(|v| v.is_object()).ok_or_else(|| {
        shape(
            format!("{tool} value missing object {field}"),
            tool,
            "value",
            value,
        )
    })
}

fn required_array<'a>(
    value: &'a Value,
    field: &str,
    tool: &str,
) -> Result<&'a [Value], RenderError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            shape(
                format!("{tool} value missing array {field}"),
                tool,
                "value",
                value,
            )
        })
}

fn diff(tool: &str, value: &Value) -> Result<String, RenderError> {
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            shape(
                format!("{tool} returned non-array files"),
                tool,
                "value",
                value,
            )
        })?;
    let mut out = String::new();
    for file in files {
        let path = required_str(file, "path", tool, "file")?;
        let label = required_str(file, "status", tool, "file")?;
        let status = DiffStatus::from_wire_label(label).ok_or_else(|| {
            shape(
                format!("{tool} file has unknown status {label:?}"),
                tool,
                "file",
                file,
            )
        })?;
        out.push_str(&format!("{path} {}\n", status.observation_label()));
        if let Some(patch) = file.get("patch").and_then(Value::as_str) {
            out.push_str(patch);
            if !patch.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// `message` and `event_id` are nullable on the wire: null prints as an empty message and `event=-`.
/// The last line is `next_cursor: <cursor or null>`, as `neige tool ls` ends its page.
fn log(tool: &str, value: &Value) -> Result<String, RenderError> {
    let commits = value
        .get("commits")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            shape(
                format!("{tool} returned non-array commits"),
                tool,
                "value",
                value,
            )
        })?;
    let mut out = String::new();
    for commit in commits {
        let hash = required_str(commit, "hash", tool, "commit")?;
        let event = match commit.get("event_id").and_then(Value::as_i64) {
            Some(id) => id.to_string(),
            None => "-".to_string(),
        };
        let message = commit.get("message").and_then(Value::as_str).unwrap_or("");
        let short = hash.get(..8).unwrap_or(hash);
        out.push_str(&format!("{short} event={event} {message}\n"));
    }
    match value.get("next_cursor") {
        Some(cursor @ (Value::Null | Value::String(_))) => {
            out.push_str(&format!("next_cursor: {cursor}\n"));
        }
        _ => {
            return Err(shape(
                format!("{tool} returned no string-or-null next_cursor"),
                tool,
                "value",
                value,
            ));
        }
    }
    Ok(out)
}

/// An empty line for an untagged report; a tag never holds whitespace, so the join is unambiguous.
fn tags(tool: &str, value: &Value) -> Result<String, RenderError> {
    let missing = || {
        shape(
            format!("{tool} value missing string array tags"),
            tool,
            "value",
            value,
        )
    };
    let tags = value
        .get("tags")
        .and_then(Value::as_array)
        .ok_or_else(missing)?;
    let tags: Vec<&str> = tags
        .iter()
        .map(|tag| tag.as_str().ok_or_else(missing))
        .collect::<Result<_, _>>()?;
    Ok(format!("{}\n", tags.join(" ")))
}

#[cfg(test)]
mod render_tests;
