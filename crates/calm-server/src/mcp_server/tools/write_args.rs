use crate::mcp_server::framing::RpcError;
use serde_json::Value;

/// The required, non-empty `message` of a write tool.
pub(crate) fn parse_write_args(args: &Value, tool: &str) -> Result<String, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    Ok(obj
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("message must be non-empty"))?
        .to_string())
}

/// The optional twin of [`parse_write_args`] for `neige_report_write`:
/// `message` may be omitted, but when present it must be a non-empty string.
pub(crate) fn parse_optional_write_args(
    args: &Value,
    tool: &str,
) -> Result<Option<String>, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    match obj.get("message") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(RpcError::invalid_params(format!(
                    "{tool}: `message` must be non-empty when provided"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
        Some(other) => Err(RpcError::invalid_params(format!(
            "{tool}: `message` must be a string, got {}",
            shape_of(other)
        ))),
    }
}

pub(crate) fn message_schema() -> Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "description": "The audit note: why this write."
    })
}

pub(crate) fn shape_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
