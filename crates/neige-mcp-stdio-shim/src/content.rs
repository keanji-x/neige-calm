//! Explicit model-visible representation of an already authorized MCP tool result.
use serde_json::{Value, json};

/// Successful result projection mode. Tool RPC failures are model-visible in every mode.
#[derive(Clone, Copy, Default)]
pub(crate) enum ResultContent {
    #[default]
    AsReceived,
    StructuredText,
    DeduplicateEquivalentContent,
}

impl ResultContent {
    /// Called only for a response correlated with an outstanding tools/call.
    pub fn tool_reply(self, line: Vec<u8>) -> Vec<u8> {
        let Ok(mut value) = serde_json::from_slice::<Value>(&line) else {
            return line;
        };
        if let Some(error) = value
            .get("error")
            .filter(|error| error.is_object())
            .cloned()
        {
            // Only correlated tools/call replies reach here. Keep the complete RPC
            // error as data and text; Claude's tool consumer otherwise sees only
            // the transport exception's message. Initialization and other methods
            // retain their original JSON-RPC envelopes.
            value.as_object_mut().expect("RPC object").remove("error");
            value["result"] = json!({
                "content": [{"type":"text", "text":error.to_string()}],
                "structuredContent": error,
                "isError": true,
            });
            let mut bytes = value.to_string().into_bytes();
            bytes.push(b'\n');
            return bytes;
        }
        if matches!(self, Self::AsReceived) {
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
        let equivalent = content.iter().any(|item| {
            item["type"] == "text"
                && item["text"].as_str().is_some_and(|text| {
                    serde_json::from_str::<Value>(text).is_ok_and(|value| &value == state)
                })
        });
        if matches!(self, Self::DeduplicateEquivalentContent) {
            if !equivalent {
                return line;
            }
            // This opt-in client has no outputSchema contract. Preserve every
            // original content block and omit only the redundant JSON value.
            result.remove("structuredContent");
        } else {
            if equivalent {
                return line;
            }
            // Keep original summaries, warnings and non-text blocks. Consumers
            // validating outputSchema still receive structuredContent unchanged.
            let mut projected = vec![json!({"type":"text","text":text})];
            projected.extend(content.iter().cloned());
            result.insert("content".into(), Value::Array(projected));
        }
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}
