//! Explicit model-visible representation of an already authorized MCP tool result.
use serde_json::{Value, json};

#[derive(Clone, Copy, Default)]
pub(crate) enum ResultContent {
    #[default]
    AsReceived,
    StructuredText,
}

impl ResultContent {
    /// Called only for a response correlated with an outstanding tools/call.
    pub fn tool_reply(self, line: Vec<u8>) -> Vec<u8> {
        if matches!(self, Self::AsReceived) {
            return line;
        }
        let Ok(mut value) = serde_json::from_slice::<Value>(&line) else {
            return line;
        };
        if value.get("error").is_some() {
            return line;
        }
        let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) else {
            return line;
        };
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return line;
        }
        let Some(state) = result
            .get("structuredContent")
            .filter(|state| state.is_object())
        else {
            return line;
        };
        let text = state.to_string();
        let Some(content) = result.get("content").and_then(Value::as_array) else {
            return line;
        };
        if content.iter().any(|item| {
            item["type"] == "text"
                && item["text"].as_str().is_some_and(|text| {
                    serde_json::from_str::<Value>(text).is_ok_and(|value| &value == state)
                })
        }) {
            return line;
        }
        // Keep original summaries, warnings and non-text blocks. Consumers
        // validating outputSchema still receive structuredContent unchanged.
        let mut projected = vec![json!({"type":"text","text":text})];
        projected.extend(content.iter().cloned());
        result.insert("content".into(), Value::Array(projected));
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}
