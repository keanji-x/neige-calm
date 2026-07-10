use crate::support;

use calm_server::worker_flow::claude_normalizer::{
    ClaudeNormalizerState, normalize_record_with_state, record_starts_turn, record_type,
};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::RawRef;
use serde_json::{Value, json};

use support::worker_flow as wf;

#[test]
fn claude_transcript_normalizer_matches_golden() {
    let cwd = "/tmp/claude-golden";
    let lines = vec![
        wf::claude_system("sys-golden", cwd),
        wf::claude_user_string("user-golden", "hello"),
        wf::claude_assistant(
            "assistant-golden",
            cwd,
            vec![
                wf::claude_thinking("thinking"),
                wf::claude_text("done"),
                wf::claude_tool_use(
                    "toolu-bash",
                    "Bash",
                    json!({ "command": "ls -la", "cwd": cwd }),
                ),
                wf::claude_tool_use(
                    "toolu-edit",
                    "Edit",
                    json!({
                        "file_path": "src/main.rs",
                        "old_string": "old",
                        "new_string": "new"
                    }),
                ),
                wf::claude_tool_use("toolu-read", "Read", json!({ "file_path": "src/main.rs" })),
                wf::claude_tool_use("toolu-web", "WebSearch", json!({ "query": "rust serde" })),
                wf::claude_tool_use("toolu-mcp", "mcp__server__tool", json!({ "arg": "value" })),
            ],
        ),
        wf::claude_user_blocks(
            "result-golden",
            vec![
                wf::claude_tool_result(
                    "toolu-bash",
                    "<stdout>total 0</stdout><stderr></stderr><exit_code>0</exit_code>",
                    false,
                ),
                wf::claude_tool_result("toolu-edit", "Applied edit", false),
                wf::claude_tool_result("toolu-web", "Found serde documentation", false),
            ],
        ),
        wf::claude_attachment("attachment-golden", cwd),
        json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "uuid": "queue-golden",
            "timestamp": "2026-06-13T00:00:05Z"
        }),
    ];
    let mut seq = 0_u64;
    let mut turn = 0_u32;
    let session_id = WorkerSessionId::from("sess-claude-golden");
    let mut state = ClaudeNormalizerState::default();
    let mut emitted = Vec::new();

    for (idx, record) in lines.into_iter().enumerate() {
        if record_starts_turn(&record) {
            turn += 1;
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Claude,
            source_path: Some("/tmp/claude-golden.jsonl".to_string()),
            line: Some(idx as u64),
            record_type: Some(record_type(&record)),
        };
        let items =
            normalize_record_with_state(&record, seq, turn, &session_id, raw_ref, &mut state);
        for item in items {
            emitted.push(serde_json::to_value(item).unwrap());
            seq += 1;
        }
    }

    assert_json_golden("expected.json", &emitted);
}

fn assert_json_golden(name: &str, actual: &[Value]) {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_transcript")
        .join(name);
    let actual_text = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());
    if std::env::var("EXPECT_FAILURES_UPDATE").ok().as_deref() == Some("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual_text).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(expected, actual_text);
}
