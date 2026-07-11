use calm_server::worker_flow::codex_normalizer::{RolloutLine, normalize_rollout_line};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{MessageBlock, RawRef, WorkerFlowItem};
use serde_json::{Value, json};

#[test]
fn message_with_unknown_content_block_still_emits_known_text() {
    let content = normalize_user_content(vec![
        json!({ "type": "input_text", "text": "hello" }),
        json!({ "type": "input_file", "file_id": "f-1" }),
    ]);

    assert_eq!(content.len(), 2);
    assert_text_block(&content[0], "hello");
    assert_text_contains(&content[1], "input_file");
}

#[test]
fn message_with_only_unknown_content_block_still_emits_placeholder() {
    let content = normalize_user_content(vec![json!({ "type": "input_file", "file_id": "f-1" })]);

    assert_eq!(content.len(), 1);
    assert_text_contains(&content[0], "input_file");
}

fn normalize_user_content(content: Vec<Value>) -> Vec<MessageBlock> {
    let value = json!({
        "timestamp": "2026-06-13T00:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": "msg-user",
            "role": "user",
            "content": content
        }
    });
    let line: RolloutLine = serde_json::from_value(value).unwrap();
    let item = normalize_rollout_line(
        &line,
        0,
        0,
        &WorkerSessionId::from("sess"),
        RawRef {
            provider: WorkerProviderKind::Codex,
            source_path: Some("/tmp/rollout.jsonl".to_string()),
            line: Some(0),
            record_type: Some("message".to_string()),
        },
    )
    .unwrap();

    let WorkerFlowItem::UserMessage { content, .. } = item else {
        panic!("expected user message");
    };
    content
}

fn assert_text_block(block: &MessageBlock, expected: &str) {
    let MessageBlock::Text { text } = block else {
        panic!("expected text block");
    };
    assert_eq!(text, expected);
}

fn assert_text_contains(block: &MessageBlock, expected: &str) {
    let MessageBlock::Text { text } = block else {
        panic!("expected text block");
    };
    assert!(
        text.contains(expected),
        "expected text block to contain {expected:?}, got {text:?}"
    );
}
