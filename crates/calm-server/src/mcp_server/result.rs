//! Typed results for kernel MCP tools. A handler's JSON is always data;
//! only these constructors create the MCP envelope and native image blocks.

use base64::Engine;
use serde::Serialize;
use serde_json::Value;

use super::framing::RpcError;

/// Bound image payloads before base64 expansion. Screenshot producers must
/// independently bound image dimensions and validate their captured PNG.
pub const MAX_TOOL_PNG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Content {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: &'static str,
    },
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

    /// Return captured PNG bytes as an image, with a separate textual and
    /// structured description. This checks the payload bound and signature;
    /// it is not a PNG decoder or a validator for untrusted uploaded images.
    pub fn png(metadata: Value, png: &[u8]) -> Result<Self, RpcError> {
        if png.len() > MAX_TOOL_PNG_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(RpcError::invalid_params("invalid or oversized tool PNG"));
        }
        let mut result = Self::structured(metadata);
        result.content.push(Content::Image {
            data: base64::engine::general_purpose::STANDARD.encode(png),
            mime_type: "image/png",
        });
        Ok(result)
    }

    /// Explicit projection for in-process consumers of a structured tool.
    /// Transport must serialize the whole result, including native images.
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

    #[test]
    fn tool_png_rejects_wrong_signature_and_oversized_payload() {
        assert!(ToolResult::png(json!({}), b"not a png").is_err());
        let mut oversized = vec![0; MAX_TOOL_PNG_BYTES + 1];
        oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        assert!(ToolResult::png(json!({}), &oversized).is_err());
    }
}
