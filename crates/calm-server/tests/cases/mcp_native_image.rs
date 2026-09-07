//! S0: image results must survive the real authenticated MCP transport.
use super::*;
use base64::Engine;
use calm_server::mcp_server::result::ToolResult;

// A valid 1x1 PNG. No model is contacted by these protocol tests.
const PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNQaPgAAAJUAZH4RZ9XAAAAAElFTkSuQmCC";

#[tokio::test]
async fn native_image_survives_authenticated_tools_call() {
    let mut registry = ToolRegistry::new();
    let handler: ToolHandler = Arc::new(|_ctx, identity, _args| {
        Box::pin(async move {
            require_role(&identity, CardRole::Planner)?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(PNG_BASE64)
                .unwrap();
            ToolResult::png(json!({ "observation_id": "frame-1" }), &png)
        })
    });
    registry.register(test_descriptor("test.terminal.observe"), handler);
    let boot = boot_with_registry(Arc::new(registry)).await;
    let response = call_with_token(
        &boot,
        &boot.raw_token,
        "test.terminal.observe",
        None,
        json!({}),
    )
    .await;
    assert!(response.get("error").is_none(), "{response:#}");
    let result = &response["result"];
    assert_eq!(result["structuredContent"]["observation_id"], "frame-1");
    let images: Vec<_> = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "image")
        .collect();
    assert_eq!(images.len(), 1, "native image block was lost: {result:#}");
    assert_eq!(images[0]["mimeType"], "image/png");
    assert_eq!(images[0]["data"], PNG_BASE64);
    assert_eq!(result["isError"], false);
    let text = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "text")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text["text"].as_str().unwrap()).unwrap(),
        json!({ "observation_id": "frame-1" })
    );

    // Adding image support must not bypass per-call thread attribution.
    let denied = call_with_token(
        &boot,
        &boot.raw_token,
        "test.terminal.observe",
        Some("unbound-thread"),
        json!({}),
    )
    .await;
    assert!(denied.get("error").is_some(), "{denied:#}");
    assert!(
        denied.get("result").is_none(),
        "image leaked on denied call: {denied:#}"
    );
    let _ = &boot.server;
}

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
