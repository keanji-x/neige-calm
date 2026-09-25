//! Text and `--json` rendering of one tool's `structuredContent`, moved from the former fat client
//! (#1801). A missing required field is a render error, never a substituted default.

use serde_json::{Value, json};

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

fn state(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    if !value.is_object() {
        return Err(shape(
            format!("{tool} returned non-object structuredContent"),
            tool,
            "value",
            value,
        ));
    }
    Ok(if json {
        compact(value)
    } else {
        format!("{value:#}\n")
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
}
