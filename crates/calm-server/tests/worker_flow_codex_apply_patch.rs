use calm_server::worker_flow::codex_normalizer::{RolloutLine, normalize_rollout_line};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{FileChangeKind, FileEdit, RawRef, WorkerFlowItem};
use serde_json::json;

#[test]
fn apply_patch_normalizer_emits_one_file_edit_per_section() {
    let patch = "\
*** Begin Patch
*** Add File: foo/a.rs
+pub fn a() {}
*** Update File: foo/b.rs
@@
-old
+new
*** Delete File: foo/c.rs
*** Update File: foo/d.rs
*** Move to: foo/d_new.rs
@@
-d
+d
*** End Patch";

    let changes = normalize_patch(patch);

    assert_eq!(changes.len(), 4);
    assert_edit(&changes[0], "foo/a.rs", "add", None, true);
    assert_edit(&changes[1], "foo/b.rs", "update", None, true);
    assert_edit(&changes[2], "foo/c.rs", "delete", None, false);
    assert_edit(
        &changes[3],
        "foo/d.rs",
        "update",
        Some("foo/d_new.rs"),
        true,
    );
}

#[test]
fn malformed_apply_patch_falls_back_to_single_patch_edit() {
    let changes = normalize_patch("*** Update File: foo/a.rs\n@@\n-old\n+new");

    assert_eq!(changes.len(), 1);
    assert_edit(&changes[0], "<patch>", "update", None, true);
}

#[test]
fn apply_patch_normalizer_preserves_end_of_file_marker() {
    let patch = "\
*** Begin Patch
*** Update File: foo.rs
@@
-last line
+new last line
*** End of File
*** End Patch";

    let changes = normalize_patch(patch);

    assert_eq!(changes.len(), 1);
    assert_edit(&changes[0], "foo.rs", "update", None, true);
    assert!(
        changes[0]
            .diff
            .as_deref()
            .is_some_and(|diff| diff.contains("*** End of File")),
        "diff = {:?}",
        changes[0].diff
    );
}

fn normalize_patch(input: &str) -> Vec<FileEdit> {
    let value = json!({
        "timestamp": "2026-06-13T00:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "call_id": "call-patch",
            "name": "apply_patch",
            "input": input,
            "status": "completed"
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
            record_type: Some("custom_tool_call".to_string()),
        },
    )
    .unwrap();

    let WorkerFlowItem::FileChange { changes, .. } = item else {
        panic!("expected file change item");
    };
    changes
}

fn assert_edit(
    edit: &FileEdit,
    path: &str,
    kind: &str,
    move_path: Option<&str>,
    expect_diff: bool,
) {
    assert_eq!(edit.path, path);
    match (&edit.kind, kind) {
        (FileChangeKind::Add, "add") => {}
        (FileChangeKind::Delete, "delete") => {}
        (FileChangeKind::Update { move_path: actual }, "update") => {
            assert_eq!(actual.as_deref(), move_path);
        }
        other => panic!("unexpected edit kind: {other:?}"),
    }
    assert_eq!(edit.diff.is_some(), expect_diff);
}
