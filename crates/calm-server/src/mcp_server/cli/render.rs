//! Text and `--json` rendering of one tool's `structuredContent`, moved from the former fat client
//! (#1801). A missing required field is a render error, never a substituted default.

use std::borrow::Cow;

use serde_json::{Value, json};

use crate::session_projection_repo::WorkerSessionState;
use crate::track_vcs::DiffStatus;

/// How one command prints its tool result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Render {
    Ls,
    /// `cat` and `cat-at`: the view content; `--json` changes only the error format.
    Content,
    State,
    Diff,
    Log,
    /// `tag`: the current tags space-joined on one line.
    Tags,
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
        Render::Ls => ls(tool, json, value),
        Render::Content => content(tool, value),
        Render::State => state(tool, json, value),
        Render::Diff if json => Ok(compact(value)),
        Render::Diff => diff(tool, value),
        Render::Log if json => Ok(compact(value)),
        Render::Log => log(tool, value),
        Render::Tags if json => Ok(compact(value)),
        Render::Tags => tags(tool, value),
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

fn ls(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    let entries = value.as_array().ok_or_else(|| {
        shape(
            format!("{tool} returned non-array structuredContent"),
            tool,
            "value",
            value,
        )
    })?;
    if json {
        return Ok(compact(value));
    }
    let mut out = String::new();
    for entry in entries {
        let name = required_str(entry, "name", tool, "entry")?;
        let kind = required_str(entry, "kind", tool, "entry")?;
        let prefix = if kind == "dir" { 'd' } else { '-' };
        out.push_str(&format!("{prefix} {name}\n"));
    }
    Ok(out)
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
const STATE_LABEL_WIDTH: usize = "lifecycle".len();

/// One fact per line, each fact once: an agent greps `^lifecycle` and gets exactly this track's.
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
    let lifecycle = required_str(track, "lifecycle", tool, "track")?;
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
    let cards = state_cards(tool, required_array(value, "cards", tool)?, &tasks)?;

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
    fact("lifecycle", lifecycle);
    fact("you", &format!("{} {}", you.id, you.role));
    fact("report", report);
    let live: Vec<&StateCard<'_>> = cards.iter().filter(|card| card.live).collect();
    for (index, line) in card_lines(&live, caller).iter().enumerate() {
        fact(if index == 0 { "live" } else { "" }, line);
    }
    Ok(out)
}

/// `(key, worker_card_id)` of each current task; only the `task <key>` suffix reads tasks.
fn state_tasks<'a>(
    tool: &str,
    tasks: &'a [Value],
) -> Result<Vec<(&'a str, Option<&'a str>)>, RenderError> {
    tasks
        .iter()
        .map(|task| {
            Ok((
                required_str(task, "key", tool, "task")?,
                nullable_str(task, "worker_card_id", tool, "task")?,
            ))
        })
        .collect()
}

struct StateCard<'a> {
    id: &'a str,
    role: &'a str,
    kind: &'a str,
    /// The runtime status, `-` without a runtime row.
    status: &'a str,
    /// The runtime is an active worker session.
    live: bool,
    /// Keys of the tasks whose worker is this card.
    tasks: Vec<&'a str>,
}

fn state_cards<'a>(
    tool: &str,
    cards: &'a [Value],
    tasks: &[(&'a str, Option<&'a str>)],
) -> Result<Vec<StateCard<'a>>, RenderError> {
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
                tasks: tasks
                    .iter()
                    .filter(|(_, worker)| *worker == Some(id))
                    .map(|(key, _)| *key)
                    .collect(),
            })
        })
        .collect()
}

/// `id  role  kind  status[  task <key>]`, columns (escaped first) padded to the widest value; the
/// caller's own row shows `(you)` for its status, which is always mid-turn.
fn card_lines(cards: &[&StateCard<'_>], caller: &str) -> Vec<String> {
    let rows: Vec<[Cow<'_, str>; 4]> = cards
        .iter()
        .map(|card| {
            let status = if card.id == caller {
                "(you)"
            } else {
                card.status
            };
            [card.id, card.role, card.kind, status].map(escape_control)
        })
        .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (id_w, role_w, kind_w, status_w) = (width(0), width(1), width(2), width(3));
    cards
        .iter()
        .zip(&rows)
        .map(|(card, [id, role, kind, status])| {
            let mut line = format!("{id:<id_w$}  {role:<role_w$}  {kind:<kind_w$}  {status}");
            if !card.tasks.is_empty() {
                let pad = status_w - status.chars().count();
                line.push_str(&format!("{:pad$}  task {}", "", card.tasks.join(", ")));
            }
            line
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

/// The field must be present: a string, or `null` for no value.
fn nullable_str<'a>(
    value: &'a Value,
    field: &str,
    tool: &str,
    what: &str,
) -> Result<Option<&'a str>, RenderError> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        _ => Err(shape(
            format!("{tool} {what} missing string-or-null {field}"),
            tool,
            what,
            value,
        )),
    }
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
        let lifecycle = required_str(commit, "lifecycle", tool, "commit")?;
        let event = match commit.get("event_id").and_then(Value::as_i64) {
            Some(id) => id.to_string(),
            None => "-".to_string(),
        };
        let message = commit.get("message").and_then(Value::as_str).unwrap_or("");
        let short = hash.get(..8).unwrap_or(hash);
        out.push_str(&format!("{short} event={event} {lifecycle} {message}\n"));
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
mod tests {
    use super::*;

    #[test]
    fn ls_entry_without_kind_is_a_render_error() {
        let ok = render(
            Render::Ls,
            "calm.track.ls",
            false,
            &json!([
                { "name": "cards/", "kind": "dir" }, { "name": "track.json", "kind": "file" }
            ]),
        );
        assert_eq!(ok.unwrap(), "d cards/\n- track.json\n");
        let err = render(
            Render::Ls,
            "calm.track.ls",
            false,
            &json!([{ "name": "x" }]),
        )
        .unwrap_err();
        assert_eq!(err.message, "calm.track.ls entry missing string kind");
    }

    #[test]
    fn diff_unknown_or_missing_status_is_a_render_error() {
        let value = json!({ "files": [{ "path": "a.md", "status": "added", "patch": "+x" }] });
        assert_eq!(
            render(Render::Diff, "calm.track.diff", false, &value).unwrap(),
            "a.md new\n+x\n"
        );
        for file in [
            json!({ "path": "a.md", "status": "renamed" }),
            json!({ "path": "a.md" }),
        ] {
            let value = json!({ "files": [file] });
            assert!(
                render(Render::Diff, "calm.track.diff", false, &value).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn log_renders_null_message_and_event_id_but_requires_lifecycle() {
        let value = json!({ "commits": [
            { "hash": "abcdef123456", "lifecycle": "working", "event_id": 42, "message": "m" },
            { "hash": "0123", "lifecycle": "draft", "event_id": null, "message": null }
        ]});
        assert_eq!(
            render(Render::Log, "calm.track.log", false, &value).unwrap(),
            "abcdef12 event=42 working m\n0123 event=- draft \n"
        );
        let value = json!({ "commits": [{ "hash": "abc", "event_id": 1, "message": "m" }] });
        assert!(render(Render::Log, "calm.track.log", false, &value).is_err());
    }

    #[test]
    fn tags_print_space_joined_and_require_a_string_array() {
        let tool = "calm.report.tag";
        let value = json!({ "tags": ["认证", "架构", "排障"] });
        assert_eq!(
            render(Render::Tags, tool, false, &value).unwrap(),
            "认证 架构 排障\n"
        );
        assert_eq!(
            render(Render::Tags, tool, true, &value).unwrap(),
            format!("{value}\n")
        );
        assert_eq!(
            render(Render::Tags, tool, false, &json!({ "tags": [] })).unwrap(),
            "\n"
        );
        for bad in [json!({}), json!({ "tags": [1] }), json!({ "tags": "a" })] {
            let err = render(Render::Tags, tool, false, &bad).unwrap_err();
            assert_eq!(
                err.message,
                "calm.report.tag value missing string array tags"
            );
        }
    }

    #[test]
    fn content_pretty_prints_json_and_otherwise_prints_raw() {
        let json_view = json!({ "content": "{\"a\":1}", "content_type": "application/json" });
        assert_eq!(
            render(Render::Content, "calm.track.cat", true, &json_view).unwrap(),
            "{\n  \"a\": 1\n}\n"
        );
        for value in [
            json!({ "content": "{not json", "content_type": "application/json" }),
            json!({ "content": "{\"a\":1}" }),
        ] {
            let raw = value["content"].as_str().unwrap();
            assert_eq!(
                render(Render::Content, "calm.track.cat", false, &value).unwrap(),
                raw
            );
        }
    }

    /// A fresh track as `calm.track.state` returns it: draft, empty title, no tasks, planner + report card.
    fn draft_track_state() -> Value {
        json!({
            "track": {
                "id": "trk_1", "area_id": "area_1", "title": "", "lifecycle": "draft",
                "cwd": "/tmp/x", "sort": 0.5, "created_at": 1, "updated_at": 2
            },
            "caller_card_id": "crd_planner",
            "cards": [
                { "id": "crd_planner", "kind": "codex", "role": "planner", "sort": 1.0,
                  "created_at": 1, "updated_at": 1,
                  "runtime": { "worker_session_id": "ws_1", "kind": "codex", "status": "running" } },
                { "id": "crd_report", "kind": "track-report", "role": "reportcard", "sort": 2.0,
                  "created_at": 1, "updated_at": 1, "runtime": null }
            ],
            "report_startup_read_required": false,
            "tasks": []
        })
    }

    /// A working track: a running worker bound to one task, an exited worker bound to another.
    fn working_track_state() -> Value {
        json!({
            "track": {
                "id": "trk_2", "area_id": "area_1", "title": "Fix login redirect",
                "lifecycle": "working", "cwd": "/tmp/y", "sort": 0.5, "created_at": 1, "updated_at": 2
            },
            "caller_card_id": "crd_planner",
            "cards": [
                { "id": "crd_planner", "kind": "codex", "role": "planner", "sort": 1.0,
                  "created_at": 1, "updated_at": 1,
                  "runtime": { "worker_session_id": "ws_1", "kind": "codex", "status": "idle" } },
                { "id": "crd_report", "kind": "track-report", "role": "reportcard", "sort": 2.0,
                  "created_at": 1, "updated_at": 1, "runtime": null },
                { "id": "crd_worker", "kind": "claude", "role": "worker", "sort": 3.0,
                  "created_at": 1, "updated_at": 1,
                  "runtime": { "worker_session_id": "ws_2", "kind": "claude", "status": "running" } },
                { "id": "crd_old", "kind": "codex", "role": "worker", "sort": 4.0,
                  "created_at": 1, "updated_at": 1,
                  "runtime": { "worker_session_id": "ws_3", "kind": "codex", "status": "exited" } }
            ],
            "report_startup_read_required": true,
            "tasks": [
                { "key": "fix-login", "status": "running", "worker_card_id": "crd_worker" },
                { "key": "add-test", "status": "pending", "worker_card_id": null },
                { "key": "old", "status": "failed", "worker_card_id": "crd_old" }
            ]
        })
    }

    fn lines_starting<'a>(text: &'a str, label: &str) -> Vec<&'a str> {
        text.lines()
            .filter(|line| line.starts_with(label))
            .collect()
    }

    #[test]
    fn state_text_of_a_draft_track_says_untitled_and_names_the_caller() {
        let text = render(
            Render::State,
            "calm.track.state",
            false,
            &draft_track_state(),
        )
        .unwrap();
        assert_eq!(
            text,
            "track      trk_1\n\
             title      (untitled)\n\
             lifecycle  draft\n\
             you        crd_planner planner\n\
             report     empty skeleton\n\
             live       crd_planner  planner  codex  (you)\n"
        );
        assert_eq!(text.lines().filter(|l| l.contains("lifecycle")).count(), 1);
        assert_eq!(text.lines().filter(|l| l.contains("draft")).count(), 1);
    }

    #[test]
    fn state_text_lists_only_live_cards_and_no_task_counts() {
        let text = render(
            Render::State,
            "calm.track.state",
            false,
            &working_track_state(),
        )
        .unwrap();
        assert_eq!(
            text,
            "track      trk_2\n\
             title      Fix login redirect\n\
             lifecycle  working\n\
             you        crd_planner planner\n\
             report     has content\n\
             live       crd_planner  planner  codex   (you)\n\
             \x20          crd_worker   worker   claude  running  task fix-login\n"
        );
        assert_eq!(text.lines().filter(|l| l.contains("lifecycle")).count(), 1);
        assert!(lines_starting(&text, "tasks").is_empty(), "{text}");
        for absent in ["crd_report", "crd_old", "exited", "old"] {
            assert!(!text.contains(absent), "{absent}: {text}");
        }
    }

    #[test]
    fn state_text_shows_a_worker_caller_as_you_and_keeps_its_task() {
        let mut value = working_track_state();
        value["caller_card_id"] = json!("crd_worker");
        let text = render(Render::State, "calm.track.state", false, &value).unwrap();
        assert_eq!(
            lines_starting(&text, "you "),
            vec!["you        crd_worker worker"]
        );
        assert_eq!(
            text.lines()
                .skip_while(|l| !l.starts_with("live"))
                .collect::<Vec<_>>(),
            vec![
                "live       crd_planner  planner  codex   idle",
                "           crd_worker   worker   claude  (you)  task fix-login",
            ]
        );
    }

    #[test]
    fn state_text_report_line_is_the_state_only() {
        let report = |value: &Value| {
            let text = render(Render::State, "calm.track.state", false, value).unwrap();
            lines_starting(&text, "report").join("\n")
        };
        let mut value = draft_track_state();
        assert_eq!(report(&value), "report     empty skeleton");
        value["report_startup_read_required"] = json!(true);
        assert_eq!(report(&value), "report     has content");
        value["cards"].as_array_mut().unwrap().pop();
        value["report_startup_read_required"] = json!(false);
        assert_eq!(report(&value), "report     none");
    }

    #[test]
    fn state_text_escapes_control_characters_so_a_title_cannot_forge_a_line() {
        let mut value = working_track_state();
        value["track"]["title"] = json!("Example\nlifecycle  done\r\t\u{7}");
        value["cards"][2]["kind"] = json!("cl\naude");
        let text = render(Render::State, "calm.track.state", false, &value).unwrap();
        assert_eq!(
            lines_starting(&text, "title"),
            vec!["title      Example\\nlifecycle  done\\r\\t\\u{7}"]
        );
        assert_eq!(
            lines_starting(&text, "lifecycle"),
            vec!["lifecycle  working"]
        );
        assert_eq!(
            text,
            "track      trk_2\n\
             title      Example\\nlifecycle  done\\r\\t\\u{7}\n\
             lifecycle  working\n\
             you        crd_planner planner\n\
             report     has content\n\
             live       crd_planner  planner  codex     (you)\n\
             \x20          crd_worker   worker   cl\\naude  running  task fix-login\n"
        );
        assert!(
            !text.trim_end_matches('\n').contains(['\r', '\t', '\u{7}']),
            "{text}"
        );
        assert!(text.contains("  cl\\naude  "), "{text}");

        value["track"]["title"] = json!("修复 登录 跳转 — café");
        let text = render(Render::State, "calm.track.state", false, &value).unwrap();
        assert_eq!(
            lines_starting(&text, "title"),
            vec!["title      修复 登录 跳转 — café"]
        );
    }

    #[test]
    fn state_json_is_the_compact_tool_result() {
        let value = working_track_state();
        assert_eq!(
            render(Render::State, "calm.track.state", true, &value).unwrap(),
            format!("{value}\n")
        );
    }

    #[test]
    fn state_shape_errors_name_the_missing_fact() {
        type Mutation = fn(&mut Value);
        let cases: [(&str, Mutation); 6] = [
            (
                "calm.track.state value missing string caller_card_id",
                |v| {
                    v.as_object_mut().unwrap().remove("caller_card_id");
                },
            ),
            ("calm.track.state track missing string lifecycle", |v| {
                v["track"].as_object_mut().unwrap().remove("lifecycle");
            }),
            ("calm.track.state value missing array tasks", |v| {
                v.as_object_mut().unwrap().remove("tasks");
            }),
            (
                "calm.track.state card missing object-or-null runtime",
                |v| {
                    v["cards"][0].as_object_mut().unwrap().remove("runtime");
                },
            ),
            (
                "calm.track.state runtime has unknown status \"exploded\"",
                |v| v["cards"][2]["runtime"]["status"] = json!("exploded"),
            ),
            (
                "calm.track.state caller card crd_gone is not among the track's cards",
                |v| v["caller_card_id"] = json!("crd_gone"),
            ),
        ];
        for (message, mutate) in cases {
            let mut value = working_track_state();
            mutate(&mut value);
            let err = render(Render::State, "calm.track.state", false, &value).unwrap_err();
            assert_eq!(err.message, message);
        }
    }
}
