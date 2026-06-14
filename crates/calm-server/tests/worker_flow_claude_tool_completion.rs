mod support;

use calm_server::worker_flow::claude_normalizer::{
    ClaudeNormalizerState, normalize_record_with_state, record_starts_turn, record_type,
};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{PatchStatus, RawRef, WorkerFlowItem};
use serde_json::{Value, json};

use support::worker_flow as wf;

#[test]
fn claude_normalizer_completes_file_change_and_web_search_tool_results() {
    let items = normalize_lines(vec![
        wf::claude_user_string("user-1", "edit and search"),
        wf::claude_assistant(
            "assistant-edit",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-edit",
                "Edit",
                json!({
                    "file_path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )],
        ),
        wf::claude_user_blocks(
            "result-edit",
            vec![wf::claude_tool_result("toolu-edit", "Applied edit", false)],
        ),
        wf::claude_assistant(
            "assistant-web",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-web",
                "WebSearch",
                json!({ "query": "rust serde" }),
            )],
        ),
        wf::claude_user_blocks(
            "result-web",
            vec![wf::claude_tool_result(
                "toolu-web",
                "Found serde documentation",
                false,
            )],
        ),
        wf::claude_assistant(
            "assistant-final",
            "/tmp/claude-tools",
            vec![wf::claude_text("done")],
        ),
    ]);

    let file_changes = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::FileChange {
                call_id,
                changes,
                status,
                ..
            } => Some((call_id.as_ref().map(|id| id.as_str()), changes, status)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(file_changes.len(), 2);
    assert_eq!(file_changes[0].0, Some("toolu-edit"));
    assert_eq!(file_changes[1].0, Some("toolu-edit"));
    assert_eq!(file_changes[0].1, file_changes[1].1);
    assert_eq!(file_changes[0].2, &PatchStatus::InProgress);
    assert_eq!(file_changes[1].2, &PatchStatus::Completed);

    let web_searches = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::WebSearch {
                call_id,
                query,
                results_summary,
                ..
            } => Some((
                call_id.as_ref().map(|id| id.as_str()),
                query.as_deref(),
                results_summary.as_deref(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(web_searches.len(), 2);
    assert_eq!(
        web_searches[0],
        (Some("toolu-web"), Some("rust serde"), None)
    );
    assert_eq!(
        web_searches[1],
        (
            Some("toolu-web"),
            Some("rust serde"),
            Some("Found serde documentation")
        )
    );
}

#[test]
fn claude_normalizer_marks_file_change_failed_when_tool_result_is_error() {
    let items = normalize_lines(vec![
        wf::claude_user_string("user-1", "edit"),
        wf::claude_assistant(
            "assistant-edit",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-edit",
                "Edit",
                json!({
                    "file_path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )],
        ),
        wf::claude_user_blocks(
            "result-edit",
            vec![wf::claude_tool_result("toolu-edit", "Edit failed", true)],
        ),
    ]);

    let statuses = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::FileChange { status, .. } => Some(status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![&PatchStatus::InProgress, &PatchStatus::Failed]
    );
}

fn normalize_lines(lines: Vec<Value>) -> Vec<WorkerFlowItem> {
    let mut seq = 0_u64;
    let mut turn = 0_u32;
    let session_id = WorkerSessionId::from("sess-claude-tools");
    let mut state = ClaudeNormalizerState::default();
    let mut emitted = Vec::new();

    for (idx, record) in lines.into_iter().enumerate() {
        if record_starts_turn(&record) {
            turn += 1;
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Claude,
            source_path: Some("/tmp/claude-tools.jsonl".to_string()),
            line: Some(idx as u64),
            record_type: Some(record_type(&record)),
        };
        let items =
            normalize_record_with_state(&record, seq, turn, &session_id, raw_ref, &mut state);
        seq = seq.saturating_add(items.len() as u64);
        emitted.extend(items);
    }

    emitted
}
