use super::*;
use calm_types::report_blocks::render_fence;
use serde_json::json;

fn builtin_body(key: &str) -> String {
    TemplateRoster::builtin()
        .get(key)
        .expect("builtin key")
        .recipe()
        .body
}

/// `PlanTaskInput` is `deny_unknown_fields`; a well-formed fence carrying `refs` must survive the read.
#[test]
fn a_wellformed_task_fence_with_task_block_vocabulary_is_silently_dropped() {
    let mut body = builtin_body(SMALL_CHANGE);
    body.push_str(&render_fence(
        KIND_TASK,
        &json!({
            "key": "with-refs",
            "kind": "codex",
            "goal": "A task that references a document.",
            "refs": ["neige://wave/w1#b_0001"],
            "ready": false,
            "declared_by": "user",
        }),
    ));
    let parsed = template_task_payloads_from_body(&body);
    let keys: Vec<&str> = parsed
        .iter()
        .filter_map(|p| p.get("key").and_then(Value::as_str))
        .collect();
    assert!(
        keys.contains(&"with-refs"),
        "a well-formed task fence carrying `refs` must survive the read; got {keys:?}"
    );
}

/// The guard's removal check is gated on `!is_tombstone(old)`, so nothing else stops a
/// rewrite from erasing a tombstone.
#[test]
fn a_task_tombstone_is_not_erased_by_the_read() {
    let mut body = builtin_body(INVESTIGATION);
    body.push_str(&render_fence(
        KIND_TASK,
        &json!({
            "key": "retired",
            "tombstone": { "reason": null },
            "declared_by": "user",
            "tombstoned_by": "user",
        }),
    ));
    let parsed = template_task_payloads_from_body(&body);
    let keys: Vec<&str> = parsed
        .iter()
        .filter_map(|p| p.get("key").and_then(Value::as_str))
        .collect();
    assert!(
        keys.contains(&"retired"),
        "a tombstone must survive the read; got {keys:?}"
    );
}
