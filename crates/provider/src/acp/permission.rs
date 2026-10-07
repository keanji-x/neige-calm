//! ACP v1 `session/request_permission`: the request an agent asks its client before a tool call,
//! and the two answers a client can give. The caller decides which answer; this module only
//! speaks the wire.

use super::Error;
use serde::Deserialize;
use serde_json::{Value, json};

/// The method name of the request.
pub const METHOD: &str = "session/request_permission";

/// The request's params, as far as a client needs them to ask a person.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub tool_call: ToolCall,
    /// The answers the agent offers, in the order it lists them.
    pub options: Vec<PermissionOption>,
}

/// The tool call the request is about. ACP sends it as a `ToolCallUpdate`, in which every field
/// but the id may be absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub locations: Option<Vec<Location>>,
}

/// A file the tool call reads or changes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Location {
    pub path: String,
}

/// One answer the agent offers. Its `kind` (`allow_once`, `allow_always`, `reject_once`,
/// `reject_always`) is the agent's to act on; a client returns only the id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

impl PermissionRequest {
    pub fn decode(params: Value) -> Result<Self, Error> {
        super::protocol::decode(params)
    }
}

/// The result that answers the request with the option `option_id`.
pub fn selected(option_id: &str) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": option_id}})
}

/// The result that answers the request without choosing; ACP requires it for every request still
/// pending once the client has sent `session/cancel`.
pub fn cancelled() -> Value {
    json!({"outcome": {"outcome": "cancelled"}})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The params OpenCode 1.18.35 sent for a `bash` call under `permission.bash = "ask"`.
    fn opencode_bash() -> Value {
        json!({
            "sessionId": "ses_1",
            "toolCall": {"toolCallId": "call_12", "title": "echo one > one.txt", "kind": "execute",
                         "status": "pending", "locations": [], "rawInput": {"command": "echo one > one.txt"}},
            "options": [
                {"optionId": "once", "kind": "allow_once", "name": "Allow once"},
                {"optionId": "always", "kind": "allow_always", "name": "Always allow"},
                {"optionId": "reject", "kind": "reject_once", "name": "Reject"}
            ]
        })
    }

    #[test]
    fn a_request_decodes_its_tool_call_and_options_in_order() {
        let request = PermissionRequest::decode(opencode_bash()).unwrap();
        assert_eq!(
            request.tool_call.title.as_deref(),
            Some("echo one > one.txt")
        );
        assert_eq!(request.tool_call.kind.as_deref(), Some("execute"));
        assert_eq!(request.tool_call.locations, Some(vec![]));
        let ids: Vec<&str> = request
            .options
            .iter()
            .map(|option| option.option_id.as_str())
            .collect();
        assert_eq!(ids, ["once", "always", "reject"]);
        let minimal = PermissionRequest::decode(json!({
            "toolCall": {"toolCallId": "t"},
            "options": [{"optionId": "a", "name": "A", "kind": "allow_once"}]
        }))
        .unwrap();
        assert_eq!(minimal.tool_call.title, None);
        assert_eq!(minimal.tool_call.locations, None);
    }

    #[test]
    fn a_request_without_a_tool_call_or_options_is_malformed() {
        for params in [
            json!({"options": []}),
            json!({"toolCall": {"toolCallId": "t"}}),
            json!({"toolCall": {"toolCallId": "t"}, "options": [{"optionId": "a", "name": "A"}]}),
        ] {
            assert!(PermissionRequest::decode(params).is_err());
        }
    }

    #[test]
    fn the_answers_are_exactly_the_acp_outcomes() {
        assert_eq!(
            selected("always").to_string(),
            r#"{"outcome":{"optionId":"always","outcome":"selected"}}"#
        );
        assert_eq!(
            cancelled().to_string(),
            r#"{"outcome":{"outcome":"cancelled"}}"#
        );
    }
}
