use crate::mcp_server::framing::RpcError;
use serde_json::{Map, Value};

/// The shared parsers ignore unknown keys, so a removed `lifecycle` would otherwise vanish
/// silently for a session that still passes it; every write tool refuses it the same way.
pub(crate) fn refuse_lifecycle_key(obj: &Map<String, Value>, tool: &str) -> Result<(), RpcError> {
    if obj.contains_key("lifecycle") {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `lifecycle` is removed: close with neige_track_close; ask with \
             neige_user_notify or neige_ratify_request"
        )));
    }
    Ok(())
}

/// A closed input (§4 of `docs/conventions/agent-commands.md`): the first key outside `valid` is
/// refused with the valid keys, so a retired key never vanishes silently.
pub(crate) fn refuse_unknown_keys(
    args: &Value,
    tool: &str,
    valid: &[&str],
) -> Result<(), RpcError> {
    let Some(obj) = args.as_object() else {
        return Err(RpcError::invalid_params(format!(
            "{tool}: arguments must be an object"
        )));
    };
    match obj.keys().find(|key| !valid.contains(&key.as_str())) {
        Some(key) => Err(RpcError::invalid_params(format!(
            "{tool}: unknown argument `{key}`; valid: {}",
            valid
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
        None => Ok(()),
    }
}

/// The required, non-empty `message` of a write tool.
pub(crate) fn parse_write_args(args: &Value, tool: &str) -> Result<String, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    refuse_lifecycle_key(obj, tool)?;
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
    refuse_lifecycle_key(obj, tool)?;
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
        "description": "Why this write; stored on its event as agent_message."
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
