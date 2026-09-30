//! A handler's JSON stays data over the real authenticated MCP transport, even when it is shaped like an envelope.
use super::*;
use calm_server::mcp_server::result::ToolResult;

// A valid 1x1 PNG. No model is contacted by these protocol tests.
const PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNQaPgAAAJUAZH4RZ9XAAAAAElFTkSuQmCC";

#[tokio::test]
async fn image_shaped_json_remains_structured_data() {
    let payload = json!({
        "content": [{ "type": "image", "data": PNG_BASE64, "mimeType": "image/png" }],
        "structuredContent": { "observation_id": "untrusted-content" },
        "isError": true
    });
    let expected = payload.clone();
    let mut registry = ToolRegistry::new();
    registry.register(
        test_descriptor("test.structured"),
        Arc::new(move |_ctx, _id, _args| {
            let payload = payload.clone();
            Box::pin(async move { Ok(ToolResult::structured(payload)) })
        }),
    );
    let boot = boot_with_registry(Arc::new(registry)).await;
    let response =
        call_with_token(&boot, &boot.raw_token, "test.structured", None, json!({})).await;
    assert_eq!(
        response["result"],
        json!({
            "content": [{ "type": "text", "text": expected.to_string() }],
            "structuredContent": expected,
            "isError": false
        })
    );
    let _ = &boot.server;
}
