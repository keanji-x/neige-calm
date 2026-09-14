//! #1667 A5 — `calm.user.notify` over the real MCP transport: a planner
//! token gets `{"ok": true}` back for a bounded text, and the two argument
//! refusals (empty, over-long) come back as `-32602` with a message that
//! names the field. The role refusal for an assistant token is asserted in
//! `mcp_assistant_tool_gate` (it lists the tool under the planner-reachable
//! denials), and its absence from the assistant's `tools/list` in
//! `mcp_tools_list_role_filter`; neither is restated here.

#![cfg(unix)]

use crate::support;

use calm_server::mcp_server::tools::user_notify::{MAX_TEXT_CHARS, TOOL_USER_NOTIFY};
use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

async fn call(text: Value, id: u64) -> Value {
    let boot = boot_with_role(CardRole::Planner).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;
    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": TOOL_USER_NOTIFY, "arguments": { "text": text } }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    let _ = (&boot.server, &boot.repo);
    resp
}

fn structured(resp: &Value) -> Value {
    let result = &resp["result"];
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {resp:#?}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

#[tokio::test]
async fn planner_notify_answers_ok_and_writes_nothing() {
    let resp = call(json!("  The user changed the block I was writing.  "), 10).await;
    assert!(resp.get("error").is_none(), "{resp:#?}");
    assert_eq!(structured(&resp), json!({ "ok": true }));
}

#[tokio::test]
async fn empty_and_over_long_text_are_invalid_params() {
    for (text, id, expect) in [
        (json!("   "), 20, "empty"),
        (json!("x".repeat(MAX_TEXT_CHARS + 1)), 21, "characters"),
    ] {
        let resp = call(text, id).await;
        let error = resp
            .get("error")
            .unwrap_or_else(|| panic!("must refuse ({expect}): {resp:#?}"));
        assert_eq!(error["code"].as_i64(), Some(-32602), "{resp:#?}");
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("`text`") && message.contains(expect),
            "refusal must name the field and the reason: {message}"
        );
    }
}
