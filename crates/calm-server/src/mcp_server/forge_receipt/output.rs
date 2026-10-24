//! Read only recorded forge output provenance; callers authorize the operation before entry.
use crate::error::{CalmError, Result};
use crate::mcp_server::result::ToolResult;
use crate::operation::{Operation, OperationOutcome, OperationResult, operation_result_from};
use serde_json::{Value, json};

const INLINE_BYTES: usize = 8192;

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
    let value = match &mut result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => result,
        _ => {
            let original = super::outcome(result);
            let wire = serde_json::to_value(&original).expect("tool result");
            let structured = &wire["structuredContent"];
            if structured.to_string().len() <= INLINE_BYTES {
                let text = calm_types::observation::receipt::escape_json(
                    &wire["content"][0]["text"].to_string(),
                );
                return ToolResult::structured_error(structured.clone(), text);
            }
            let full = structured.to_string();
            let bounded = json!({"op_id":op_id,"parked":false,"status":structured["status"],
                "outcome":structured["outcome"],"output_ref":format!("ops/{op_id}/result"),
                "rev":crate::report_sources::sha256_hex(full.as_bytes()),"bytes":full.len(),
                "error_preview":calm_types::observation::receipt::preview(&structured["error"])});
            let text = calm_types::observation::receipt::escape_json(&bounded.to_string());
            return ToolResult::structured_error(bounded, text);
        }
    };
    match text(value).await {
        Ok(text) => {
            let reference = format!("ops/{op_id}/result");
            if text.len() > INLINE_BYTES || value.to_string().len() > INLINE_BYTES {
                *value = json!({"output_ref":reference,"bytes":text.len(),"rev":crate::report_sources::sha256_hex(text.as_bytes())});
            } else if value.pointer("/event/artifact_path").is_some() {
                value["stdout"] = Value::String(text);
            }
        }
        Err(_) => {
            if value.to_string().len() > INLINE_BYTES {
                *value = json!({"output_unavailable":true,
                    "metadata_preview":calm_types::observation::receipt::preview(value)});
            } else {
                value["output_unavailable"] = json!(true);
            }
        }
    }
    let wire = super::outcome(result).into_structured();
    let summary = calm_types::observation::receipt::escape_json(&wire.to_string());
    ToolResult::structured_with_summary(wire, summary)
}
