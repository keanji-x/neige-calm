//! Read only recorded forge output provenance; callers authorize the operation before entry.
use crate::error::{CalmError, Result};
use crate::mcp_server::result::ToolResult;
use crate::operation::{Operation, OperationOutcome, OperationResult, operation_result_from};
use serde_json::{Value, json};

const INLINE_BYTES: usize = 16 * 1024;

fn succeeded_value(outcome: &OperationOutcome) -> Option<&Value> {
    match outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => Some(result),
        _ => None,
    }
}

/// The stdout stored in the outcome wins. Otherwise use only artifact provenance recorded
/// by the forge adapter, never a caller-supplied disk path or a derived private filename.
pub(super) async fn text(value: &Value) -> Result<String> {
    if let Some(stdout) = value.get("stdout").and_then(Value::as_str) {
        return Ok(stdout.to_owned());
    }
    if let Some(path) = value
        .pointer("/event/artifact_path")
        .and_then(Value::as_str)
    {
        return tokio::fs::read_to_string(path).await.map_err(|_| {
            CalmError::BadRequest("recorded forge output artifact is unavailable".into())
        });
    }
    Ok(serde_json::to_string_pretty(value)?)
}

pub(crate) async fn read(op: &Operation) -> Result<String> {
    let result = operation_result_from(op)?
        .ok_or_else(|| CalmError::BadRequest("forge output is pending".into()))?;
    match succeeded_value(&result.outcome) {
        Some(value) => text(value).await,
        None => Ok(super::outcome(result).into_structured().to_string()),
    }
}

pub(crate) async fn receipt(mut result: OperationResult) -> ToolResult {
    let op_id = result.op_id.clone();
    let mut output_bytes = None;
    let mut unavailable = false;
    match &mut result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => match text(result).await {
            Ok(text) => {
                output_bytes = Some(text.len());
                if result.pointer("/event/artifact_path").is_some() {
                    result["stdout"] = Value::String(text);
                }
            }
            Err(_) => {
                unavailable = true;
                result["output_unavailable"] = json!(true);
            }
        },
        _ => {}
    }
    let original = super::outcome(result);
    let original = serde_json::to_value(original).expect("tool result");
    let is_error = original["isError"] == true;
    let mut wire = original["structuredContent"].clone();
    let full = wire.to_string();
    // Budget the displayed value after escaping, including the outcome envelope and
    // collision metadata. The overflow reader retains the original stored outcome.
    if calm_types::observation::receipt::escape_json(&full).len() > INLINE_BYTES {
        let reference = json!({"output_ref":format!("ops/{op_id}/result"),
            "bytes":output_bytes.unwrap_or(full.len())});
        if is_error {
            wire = json!({"op_id":op_id,"parked":false,"status":wire["status"],
                "outcome":wire["outcome"],"output_ref":reference["output_ref"],
                "bytes":reference["bytes"],
                "error_preview":calm_types::observation::receipt::preview(&wire["error"])});
        } else {
            wire["result"] = if unavailable {
                json!({"output_unavailable":true})
            } else {
                reference
            };
        }
    }
    let summary = calm_types::observation::receipt::escape_json(&wire.to_string());
    if is_error {
        ToolResult::structured_error(wire, summary)
    } else {
        ToolResult::structured_with_summary(wire, summary)
    }
}
