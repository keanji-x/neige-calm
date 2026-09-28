//! The Claude CLI's model list (#1822): one `initialize` control request is written to
//! `<claude_binary> -p`, stdin is closed, and the CLI answers with a `control_response` whose
//! `response.models` is its own `/model` list, then exits. No user message is sent, so no model is
//! called. Only `models` is decoded: every other key of the answer (the account among them) is
//! skipped by the decoder. A refusal quotes at most a value from inside the `models` list (a
//! repeated `value` or effort level), never anything outside it, so never the account; nothing
//! outside the list reaches a reason, a log line or the cache.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::models::{ClaudeCatalog, ClaudeModel};
use super::readiness_command;

/// How long the exchange may take; the pinned CLI answers in about 4 s.
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);

/// The `request_id` of the one control request, echoed by its answer.
const REQUEST_ID: &str = "neige-model-catalog";

/// The argv of the exchange. `--setting-sources project` and `settings` are the turn spawn's own,
/// so a settings-level model restriction the turn would meet is in the list. The rest keep the
/// run to the answer: no MCP server, no slash commands, and no session written.
pub(crate) fn argv(settings: &str) -> Vec<&str> {
    vec![
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--settings",
        settings,
    ]
}

fn initialize_line() -> String {
    let mut line = serde_json::json!({
        "type": "control_request",
        "request_id": REQUEST_ID,
        "request": {"subtype": "initialize"},
    })
    .to_string();
    line.push('\n');
    line
}

/// Run the exchange with `env` (the spawn's own allowlisted environment, readiness marker
/// included) in `cwd` through [`readiness_command::run_with_input`], and parse the list strictly.
/// `Err` is neige's own sentence for why there is no list.
pub async fn fetch(
    binary: &Path,
    env: &[(String, OsString)],
    cwd: &Path,
    timeout: Duration,
) -> Result<ClaudeCatalog, String> {
    let command_name = format!("{} -p (initialize)", binary.display());
    let settings = super::spawn::settings_json();
    let line = initialize_line();
    let (status, stdout) = readiness_command::run_with_input(
        binary,
        &argv(&settings),
        readiness_command::Input {
            stdin: line.as_bytes(),
            cwd,
        },
        env,
        timeout,
    )
    .await
    .map_err(|failure| format!("{command_name} {failure}"))?;
    if !status.success() {
        return Err(format!("{command_name} exited with {status}"));
    }
    let entries = parse(&stdout)
        .map_err(|why| format!("{command_name} printed no usable model list: {why}"))?;
    ClaudeCatalog::from_entries(entries, crate::model::now_ms())
        .map_err(|why| format!("{command_name} printed no usable model list: it {why}"))
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ResponseHead {
    response: Head,
}

#[derive(Deserialize)]
struct Head {
    request_id: String,
    subtype: String,
}

#[derive(Deserialize)]
struct InitializeResponse {
    response: InitializeEnvelope,
}

#[derive(Deserialize)]
struct InitializeEnvelope {
    response: InitializeAnswer,
}

/// The one key neige reads from the answer.
#[derive(Deserialize)]
struct InitializeAnswer {
    models: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    value: String,
    resolved_model: String,
    display_name: String,
    description: String,
    /// Absent when the CLI declares no effort for the model.
    supported_effort_levels: Option<Vec<String>>,
}

/// The entries of the one answer to [`REQUEST_ID`] in the CLI's stream-json `stdout`. `Err` is
/// neige's own words, quoting at most a value from inside the `models` list.
fn parse(stdout: &[u8]) -> Result<Vec<ClaudeModel>, String> {
    let text = std::str::from_utf8(stdout).map_err(|_| "its output is not UTF-8".to_string())?;
    let mut answer = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: Record =
            serde_json::from_str(line).map_err(|_| "a line of its output is not a record")?;
        if record.kind != "control_response" {
            continue;
        }
        let head: ResponseHead = serde_json::from_str(line)
            .map_err(|_| "a control response carries no request id and subtype")?;
        if head.response.request_id != REQUEST_ID {
            continue;
        }
        if head.response.subtype != "success" {
            return Err("it answered `initialize` with an error".into());
        }
        let parsed: InitializeResponse = serde_json::from_str(line)
            .map_err(|_| "its `initialize` answer carries no well-formed `models` list")?;
        if answer.replace(parsed.response.response.models).is_some() {
            return Err("it answered `initialize` twice".into());
        }
    }
    let entries = answer.ok_or_else(|| "it never answered `initialize`".to_string())?;
    entries
        .into_iter()
        .enumerate()
        .map(|(at, entry)| model(entry).map_err(|why| format!("entry {at} {why}")))
        .collect()
}

fn model(entry: Entry) -> Result<ClaudeModel, String> {
    for (field, value) in [
        ("value", &entry.value),
        ("resolvedModel", &entry.resolved_model),
        ("displayName", &entry.display_name),
    ] {
        if value.trim().is_empty() {
            return Err(format!("has an empty `{field}`"));
        }
    }
    let effort_levels = entry.supported_effort_levels.unwrap_or_default();
    for (at, level) in effort_levels.iter().enumerate() {
        if level.trim().is_empty() {
            return Err("declares an empty effort level".into());
        }
        if effort_levels[..at].contains(level) {
            return Err(format!("declares effort level `{level}` twice"));
        }
    }
    Ok(ClaudeModel {
        value: entry.value,
        resolved_model: entry.resolved_model,
        display_name: entry.display_name,
        description: entry.description,
        effort_levels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(models: serde_json::Value) -> String {
        serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": REQUEST_ID,
                "response": {
                    "commands": [],
                    "models": models,
                    "account": {"email": "owner@example.invalid"},
                },
            },
        })
        .to_string()
    }

    fn entry(value: &str) -> serde_json::Value {
        serde_json::json!({
            "value": value, "resolvedModel": format!("r-{value}"), "displayName": value,
            "description": "d", "supportsEffort": true, "supportedEffortLevels": ["low", "high"],
        })
    }

    #[test]
    fn the_answer_to_our_request_is_the_list_and_other_records_are_skipped() {
        let haiku = serde_json::json!({
            "value": "haiku", "resolvedModel": "claude-haiku-4-5", "displayName": "Haiku",
            "description": "fast",
        });
        let other = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": "someone-else", "response": {}},
        });
        let stdout = format!(
            "{}\n{other}\n{}\n",
            r#"{"type":"system","subtype":"status"}"#,
            answer(serde_json::json!([entry("default"), haiku]))
        );
        let got = parse(stdout.as_bytes()).expect("a list");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].effort_levels, ["low", "high"]);
        assert_eq!(got[1].resolved_model, "claude-haiku-4-5");
        assert!(got[1].effort_levels.is_empty(), "absent levels: none");
    }

    #[test]
    fn every_malformed_answer_is_refused_without_quoting_it() {
        let mut missing_resolved = entry("sonnet");
        missing_resolved
            .as_object_mut()
            .unwrap()
            .remove("resolvedModel");
        let mut twice = entry("sonnet");
        twice["supportedEffortLevels"] = serde_json::json!(["low", "low"]);
        let error = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": REQUEST_ID, "error": "owner@example.invalid"},
        });
        for stdout in [
            String::new(),
            "not json owner@example.invalid".to_string(),
            error.to_string(),
            answer(serde_json::json!("owner@example.invalid")),
            answer(serde_json::json!([missing_resolved])),
            answer(serde_json::json!([entry("")])),
            answer(serde_json::json!([twice])),
            format!(
                "{}\n{}",
                answer(serde_json::json!([])),
                answer(serde_json::json!([]))
            ),
        ] {
            let why = parse(stdout.as_bytes()).expect_err(&stdout);
            assert!(!why.contains("owner@example.invalid"), "{why}");
        }
    }
}
