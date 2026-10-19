//! Selective codecs for Claude discovery exchanges. Identity and account fields are skipped.

use serde::Deserialize;

/// The request id echoed by the catalog initialize response.
const REQUEST_ID: &str = "neige-model-catalog";

/// Decode only the boolean answer; missing or malformed booleans remain unknown.
pub fn decode_auth_status(bytes: &[u8]) -> Result<bool, String> {
    #[derive(Deserialize)]
    struct AuthStatus {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
    }
    serde_json::from_slice::<AuthStatus>(bytes)
        .map(|status| status.logged_in)
        .map_err(|_| "printed no boolean `loggedIn`".to_string())
}

fn effort_levels<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(decoder)?.unwrap_or_default())
}

/// The `value` of the CLI's own default entry. It is the `null` selection, never a stored one.
const DEFAULT_VALUE: &str = "default";

/// One entry of the CLI's model list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeModel {
    /// The CLI's `value`: what `--model` is given, verbatim.
    pub value: String,
    /// The CLI's `resolvedModel`: the model this entry runs.
    pub resolved_model: String,
    pub display_name: String,
    pub description: String,
    /// The CLI's `supportedEffortLevels`, in its order; empty when it declares none, and then no
    /// effort is accepted.
    #[serde(
        rename = "supportedEffortLevels",
        default,
        deserialize_with = "effort_levels"
    )]
    pub effort_levels: Vec<String>,
}

/// The CLI's model list as one `initialize` answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCatalog {
    /// The CLI's [`DEFAULT_VALUE`] entry: what the `null` selection runs, and its effort levels.
    pub default: ClaudeModel,
    /// Every other entry, in the CLI's order: the values a selection may name.
    pub models: Vec<ClaudeModel>,
}

impl ClaudeCatalog {
    /// The catalog of the CLI's entries: exactly one [`DEFAULT_VALUE`], at least one other entry,
    /// and no value twice. `Err` says which rule the list breaks.
    fn from_entries(entries: Vec<ClaudeModel>) -> Result<Self, String> {
        let mut default = None;
        let mut models: Vec<ClaudeModel> = Vec::new();
        for entry in entries {
            if entry.value == DEFAULT_VALUE {
                if default.replace(entry).is_some() {
                    return Err(format!("lists `{DEFAULT_VALUE}` twice"));
                }
            } else if models.iter().any(|m| m.value == entry.value) {
                return Err(format!("lists `{}` twice", entry.value));
            } else {
                models.push(entry);
            }
        }
        let default = default.ok_or_else(|| format!("lists no `{DEFAULT_VALUE}` entry"))?;
        if models.is_empty() {
            return Err("lists no model besides the default".into());
        }
        Ok(Self { default, models })
    }
}

/// One initialize control request, including its stream-json newline; no user prompt.
pub fn catalog_initialize_line() -> String {
    let mut line = serde_json::json!({
        "type": "control_request",
        "request_id": REQUEST_ID,
        "request": {"subtype": "initialize"},
    })
    .to_string();
    line.push('\n');
    line
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
    models: Vec<ClaudeModel>,
}

/// The entries of the one answer to [`REQUEST_ID`] in the CLI's stream-json `stdout`. `Err` is
/// neige's own words, quoting at most a value from inside the `models` list.
pub fn decode_catalog(stdout: &[u8]) -> Result<ClaudeCatalog, String> {
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
    let entries = entries
        .into_iter()
        .enumerate()
        .map(|(at, entry)| model(entry).map_err(|why| format!("entry {at} {why}")))
        .collect::<Result<Vec<_>, _>>()?;
    ClaudeCatalog::from_entries(entries).map_err(|why| format!("it {why}"))
}

fn model(entry: ClaudeModel) -> Result<ClaudeModel, String> {
    for (field, value) in [
        ("value", &entry.value),
        ("resolvedModel", &entry.resolved_model),
        ("displayName", &entry.display_name),
    ] {
        if value.trim().is_empty() {
            return Err(format!("has an empty `{field}`"));
        }
    }
    let effort_levels = &entry.effort_levels;
    for (at, level) in effort_levels.iter().enumerate() {
        if level.trim().is_empty() {
            return Err("declares an empty effort level".into());
        }
        if effort_levels[..at].contains(level) {
            return Err(format!("declares effort level `{level}` twice"));
        }
    }
    Ok(entry)
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
        let got = decode_catalog(stdout.as_bytes()).expect("a list");
        assert_eq!(got.models.len(), 1);
        assert_eq!(got.default.effort_levels, ["low", "high"]);
        assert_eq!(got.models[0].resolved_model, "claude-haiku-4-5");
        assert!(
            got.models[0].effort_levels.is_empty(),
            "absent levels: none"
        );
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
            let why = decode_catalog(stdout.as_bytes()).expect_err(&stdout);
            assert!(!why.contains("owner@example.invalid"), "{why}");
        }
    }
}
