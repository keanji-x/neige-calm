use crate::mcp_server::framing::RpcError;
use crate::model::TrackLifecycle;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct WriteArgs {
    pub message: String,
    pub lifecycle: Option<TrackLifecycle>,
}

pub(crate) fn parse_write_args(args: &Value, tool: &str) -> Result<WriteArgs, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;

    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("message must be non-empty"))?
        .to_string();

    let lifecycle = match obj.get("lifecycle") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(parse_lifecycle_name(s, tool)?),
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `lifecycle` must be a string, got {}",
                shape_of(other)
            )));
        }
    };

    Ok(WriteArgs { message, lifecycle })
}

pub(crate) fn parse_lifecycle_name(s: &str, tool: &str) -> Result<TrackLifecycle, RpcError> {
    match s {
        "draft" => Ok(TrackLifecycle::Draft),
        "planning" => Ok(TrackLifecycle::Planning),
        "dispatching" => Ok(TrackLifecycle::Dispatching),
        "working" => Ok(TrackLifecycle::Working),
        "blocked" => Ok(TrackLifecycle::Blocked),
        "reviewing" => Ok(TrackLifecycle::Reviewing),
        "done" => Ok(TrackLifecycle::Done),
        "canceled" => Ok(TrackLifecycle::Canceled),
        "failed" => Ok(TrackLifecycle::Failed),
        other => Err(RpcError::invalid_params(format!(
            "{tool}: unknown lifecycle `{other}`. Allowed: draft, planning, \
             dispatching, working, blocked, reviewing, done, canceled, failed."
        ))),
    }
}

pub(crate) fn lifecycle_schema() -> Value {
    serde_json::json!({
        "type": "string",
        "enum": [
            "draft", "planning", "dispatching", "working",
            "blocked", "reviewing", "done", "canceled", "failed"
        ],
        "description": "Optional track lifecycle transition. Use planning after \
            understanding the goal, dispatching when requesting workers, working \
            when work is underway, blocked when user input is needed, reviewing \
            when validating worker results, done after acceptance, or failed \
            when completion is impossible. Omit to leave lifecycle unchanged."
    })
}

pub(crate) fn message_schema() -> Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "description": "Required human-readable rationale for this write. The \
            kernel persists it on the emitted event as agent_message and on \
            TrackUpdated.agent_message when a lifecycle transition is requested."
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
