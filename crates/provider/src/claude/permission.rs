//! The `can_use_tool` control request the CLI sends under `--permission-prompt-tool stdio`, and
//! the decision it is answered with (#2348).
//!
//! Only the fields a person is shown are read. `permission_suggestions` is not: a decision never
//! carries `updatedPermissions` (nor `updatedInput`), so no answer can change the permission mode
//! or add a rule while the CLI runs.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::protocol::{ControlResponseOut, ControlResponseOutBody, ProtocolError};

/// The control request subtype that asks whether a tool may run.
pub const CAN_USE_TOOL: &str = "can_use_tool";

/// One `can_use_tool` request, as far as a person is shown it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CanUseTool {
    pub tool_name: String,
    pub input: Map<String, Value>,
    /// The path a sandbox or a rule blocked, when the CLI names one.
    #[serde(default)]
    pub blocked_path: Option<String>,
    /// The CLI's own account of why it asks, when it gives one.
    #[serde(default)]
    pub decision_reason: Option<String>,
}

impl CanUseTool {
    /// The request a `control_request` carries: `Ok(None)` for another subtype, `Err` for a
    /// `can_use_tool` without its recorded shape.
    pub fn from_request(request: &Value) -> Result<Option<Self>, ProtocolError> {
        if request.get("subtype").and_then(Value::as_str) != Some(CAN_USE_TOOL) {
            return Ok(None);
        }
        serde_json::from_value(request.clone())
            .map(Some)
            .map_err(|source| ProtocolError::Shape {
                kind: format!("control_request/{CAN_USE_TOOL}"),
                source,
            })
    }
}

/// The answer to one `can_use_tool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Run the tool with the input it asked with.
    Allow,
    /// Do not run it; the CLI hands `message` to the model as the tool's error result.
    Deny { message: String },
}

/// The `control_response` line that answers `request_id` with `decision`.
pub fn permission_response(request_id: &str, decision: &PermissionDecision) -> ControlResponseOut {
    ControlResponseOut::new(ControlResponseOutBody::Success {
        request_id: request_id.to_string(),
        response: serde_json::to_value(decision).expect("a permission decision serializes"),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_decision_is_exactly_its_behavior_and_nothing_the_cli_suggested() {
        let allow =
            serde_json::to_string(&permission_response("r-1", &PermissionDecision::Allow)).unwrap();
        assert_eq!(
            allow,
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"r-1","response":{"behavior":"allow"}}}"#
        );
        let deny = serde_json::to_string(&permission_response(
            "r-2",
            &PermissionDecision::Deny {
                message: "no".into(),
            },
        ))
        .unwrap();
        assert_eq!(
            deny,
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"r-2","response":{"behavior":"deny","message":"no"}}}"#
        );
    }

    #[test]
    fn a_can_use_tool_reads_what_a_person_is_shown() {
        let request = json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "input": {"command": "cargo test"},
            "permission_suggestions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}],
            "blocked_path": "/ws/.git",
            "decision_reason": "outside the sandbox",
            "tool_use_id": "toolu_1",
        });
        let parsed = CanUseTool::from_request(&request).unwrap().unwrap();
        assert_eq!(parsed.tool_name, "Bash");
        assert_eq!(parsed.input["command"], "cargo test");
        assert_eq!(parsed.blocked_path.as_deref(), Some("/ws/.git"));
        assert_eq!(
            parsed.decision_reason.as_deref(),
            Some("outside the sandbox")
        );
        assert!(
            CanUseTool::from_request(&json!({"subtype": "interrupt"}))
                .unwrap()
                .is_none()
        );
        assert!(CanUseTool::from_request(&json!({"subtype": "can_use_tool"})).is_err());
    }
}
