//! Public transcript payload vocabulary, aligned with core/domain/conversation.ts.
//! Unknown provider fields and item types publish only activity metadata.
use super::TranscriptRow;
use crate::mail::TOOL_MAIL_SEND;
use crate::mcp_server::tools::user_ask::TOOL_USER_ASK;
use serde_json::{Value, json};

pub(super) fn params(row: &TranscriptRow) -> String {
    let Ok(raw) = serde_json::from_str::<Value>(&row.params) else {
        return "{}".into();
    };
    let mut out = json!({});
    if row.method == "turn/completed" {
        strings(&raw, &mut out, &["id", "status"]);
        numbers(&raw, &mut out, &["durationMs"]);
        if raw["error"].is_object() {
            let mut error = json!({});
            strings(&raw["error"], &mut error, &["message"]);
            // The renderer consumes only the error variant name, never its payload.
            match &raw["error"]["codexErrorInfo"] {
                Value::String(code) => error["codexErrorInfo"] = json!(code),
                Value::Object(info) => {
                    error["codexErrorInfo"] =
                        Value::Object(info.keys().map(|key| (key.clone(), Value::Null)).collect());
                }
                _ => {}
            }
            out["error"] = error;
        }
        match &raw["harness_interruption_reason"] {
            Value::String(reason) if reason == "max_turn_duration" => {
                out["harness_interruption_reason"] = json!(reason);
            }
            Value::Object(_) => {
                let evidence = &raw["harness_interruption_reason"];
                let mut safe = json!({"request":{}});
                strings(evidence, &mut safe, &["version"]);
                numbers(evidence, &mut safe, &["ended_at_ms"]);
                strings(
                    &evidence["request"],
                    &mut safe["request"],
                    &[
                        "session_id",
                        "card_id",
                        "track_id",
                        "thread_id",
                        "turn_id",
                        "reason",
                    ],
                );
                numbers(
                    &evidence["request"],
                    &mut safe["request"],
                    &["requested_at_ms", "effective_elapsed_ms", "budget_ms"],
                );
                out["harness_interruption_reason"] = safe;
            }
            _ => {}
        }
    } else if raw["item"].is_object() {
        numbers(&raw, &mut out, &["completedAtMs"]);
        let item = &raw["item"];
        let mut safe = json!({});
        strings(item, &mut safe, &["id", "type", "status"]);
        numbers(item, &mut safe, &["durationMs", "exitCode"]);
        // Preserve the failure signal without publishing tool error details.
        if !item["error"].is_null() {
            safe["error"] = json!(true);
        }
        match row.item_type.as_deref() {
            Some("agentMessage" | "agent_message") => strings(item, &mut safe, &["text"]),
            Some("userMessage" | "user_message") => {
                if let Some(content) = item["content"].as_array() {
                    safe["content"] = content.iter().filter_map(user_part).collect();
                }
            }
            Some("fileChange") => {
                if let Some(changes) = item["changes"].as_array() {
                    // The activity contract consumes the length, never paths or diffs.
                    safe["changes"] = changes.iter().map(|_| json!({})).collect();
                }
            }
            Some("mcpToolCall") => {
                strings(item, &mut safe, &["tool"]);
                match item["tool"].as_str() {
                    Some(TOOL_USER_ASK) => {
                        if let Some(questions) = item["arguments"]["questions"].as_array() {
                            safe["arguments"] = json!({"questions":questions.iter().map(|question| {
                                let mut safe = json!({});
                                strings(question, &mut safe, &["title"]);
                                if let Some(options) = question["options"].as_array() {
                                    safe["options"] = options.iter().filter(|v|v.is_string()).cloned().collect();
                                }
                                safe
                            }).collect::<Vec<_>>()});
                        }
                    }
                    Some(TOOL_MAIL_SEND) => {
                        let mut args = json!({});
                        strings(&item["arguments"], &mut args, &["summary"]);
                        safe["arguments"] = args;
                    }
                    _ => {}
                }
            }
            Some("dynamicToolCall") => strings(item, &mut safe, &["tool"]),
            // commandExecution, reasoning and all other activities need only metadata.
            _ => {}
        }
        out["item"] = safe;
    }
    out.to_string()
}

fn user_part(part: &Value) -> Option<Value> {
    let mut out = json!({});
    strings(part, &mut out, &["type"]);
    match part["type"].as_str()? {
        "inputText" | "text" => strings(part, &mut out, &["text"]),
        "localImage" => {
            out["path"] = json!(crate::planner_attachments::REDACTED_LOCAL_IMAGE_PATH);
        }
        "image" | "inputImage" => strings(part, &mut out, &["url", "imageUrl"]),
        _ => return None,
    }
    Some(out)
}

fn strings(raw: &Value, out: &mut Value, keys: &[&str]) {
    for key in keys {
        if raw[*key].is_string() {
            out[*key] = raw[*key].clone();
        }
    }
}

fn numbers(raw: &Value, out: &mut Value, keys: &[&str]) {
    for key in keys {
        if raw[*key].is_number() {
            out[*key] = raw[*key].clone();
        }
    }
}
