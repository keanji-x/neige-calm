//! Typed results for kernel MCP tools. A handler's JSON is always data;
//! only these constructors create the MCP envelope.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Content {
    Text { text: String },
}

/// One successful tools/call result. RPC failures remain `RpcError` and are
/// handled by the existing transport error path. Fields are private so a
/// structured payload cannot accidentally masquerade as an envelope.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    content: Vec<Content>,
    structured_content: Value,
    is_error: bool,
}

impl ToolResult {
    /// Preserve the established JSON-only tool wire contract exactly.
    pub fn structured(value: Value) -> Self {
        let text = value.to_string();
        Self {
            content: vec![Content::Text { text }],
            structured_content: value,
            is_error: false,
        }
    }

    /// One text block carrying only `summary`; the complete state lives in
    /// `structuredContent`. For tools whose state would otherwise be delivered
    /// twice to a model that reads the raw result verbatim.
    pub fn structured_with_summary(value: Value, summary: String) -> Self {
        Self {
            content: vec![Content::Text { text: summary }],
            structured_content: value,
            is_error: false,
        }
    }

    /// Explicit projection for in-process consumers of a structured tool.
    /// Transport must serialize the whole result.
    pub fn into_structured(self) -> Value {
        self.structured_content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_result_carries_state_once() {
        let result = ToolResult::structured_with_summary(
            json!({"text":["SECRET_SCREEN"],"n":1}),
            "one line".into(),
        );
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["content"], json!([{"type":"text","text":"one line"}]));
        assert_eq!(wire["structuredContent"]["n"], 1);
        assert!(!wire["content"].to_string().contains("SECRET_SCREEN"));
        assert_eq!(
            serde_json::to_value(ToolResult::structured(json!({"n":1}))).unwrap()["content"][0]["text"],
            "{\"n\":1}",
            "other tools keep the JSON text projection"
        );
    }
}
