mod support;

use calm_server::worker_flow::codex_normalizer::{
    RolloutLine, is_turn_context, normalize_rollout_line, rollout_record_type,
};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::RawRef;
use serde_json::Value;

use support::worker_flow as wf;

#[test]
fn codex_rollout_normalizer_matches_golden() {
    let thread_id = "thread-golden";
    let lines = vec![
        wf::session_meta(thread_id),
        wf::turn_context("turn-1"),
        wf::user_message("msg-user", "hello"),
        wf::reasoning("rsn-1", "thinking"),
        wf::assistant_message("msg-assistant", "done"),
        wf::function_call("call-exec", "ls -la"),
        wf::function_output("call-exec", "total 0"),
        wf::custom_patch("call-patch"),
        wf::web_search("ws-1", "rust serde flatten"),
        wf::compacted("summary after compaction"),
    ];
    let mut seq = 0_u64;
    let mut turn = 0_u32;
    let session_id = WorkerSessionId::from("sess-golden");
    let mut emitted = Vec::new();

    for (idx, value) in lines.into_iter().enumerate() {
        let line: RolloutLine = serde_json::from_value(value).unwrap();
        if is_turn_context(&line) {
            turn += 1;
            continue;
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Codex,
            source_path: Some("/tmp/rollout-golden.jsonl".to_string()),
            line: Some(idx as u64),
            record_type: Some(rollout_record_type(&line).to_string()),
        };
        if let Some(item) = normalize_rollout_line(&line, seq, turn, &session_id, raw_ref) {
            emitted.push(serde_json::to_value(item).unwrap());
            seq += 1;
        }
    }

    assert_json_golden("expected.json", &emitted);
}

fn assert_json_golden(name: &str, actual: &[Value]) {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex_rollout")
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
