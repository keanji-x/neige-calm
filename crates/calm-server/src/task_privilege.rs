//! Privilege-field normalization for task blocks copied into a track that did not author them
//! (fork and recipes). Track-scoped fields (`refs`, ids, links, tombstones) are NOT handled here.

use serde_json::{Map, Value};

/// Strip authority inherited from the source track. `released_by_user` is removed rather
/// than set `false`: `track_report_edit_guard` compares the raw `Option<&Value>`. An illegal
/// `tombstoned_by` on a live block is left for `validate_payload` to reject.
pub(crate) fn normalize_task_privilege_fields(payload: &mut Map<String, Value>) {
    let tombstone = payload
        .get("tombstone")
        .is_some_and(|value| !value.is_null());
    // `"spec"` is frozen document vocabulary (`validate_declared_by` accepts only `"spec" | "user"`),
    // not the kernel's actor name; changing it here alone deadlocks the two guards against each other.
    payload.insert("declared_by".into(), Value::String("spec".into()));
    if tombstone {
        payload.insert("tombstoned_by".into(), Value::String("spec".into()));
        payload.remove("ready");
    } else {
        payload.insert("ready".into(), Value::Bool(false));
        payload.remove("released_by_user");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    #[test]
    fn live_task_loses_user_authorship_and_release() {
        let mut payload = map(json!({
            "key": "k",
            "goal": "g",
            "declared_by": "user",
            "ready": true,
            "released_by_user": true,
        }));
        normalize_task_privilege_fields(&mut payload);
        assert_eq!(payload["declared_by"], json!("spec"));
        assert_eq!(payload["ready"], json!(false));
        // Absent, not `false`: the guard compares the raw Option.
        assert!(
            !payload.contains_key("released_by_user"),
            "must be removed, not set false: {payload:?}"
        );
    }

    #[test]
    fn tombstone_keeps_no_ready_and_is_re_signed() {
        let mut payload = map(json!({
            "key": "k",
            "tombstone": { "reason": null },
            "declared_by": "user",
            "tombstoned_by": "user",
            "ready": true,
        }));
        normalize_task_privilege_fields(&mut payload);
        assert_eq!(payload["declared_by"], json!("spec"));
        assert_eq!(payload["tombstoned_by"], json!("spec"));
        assert!(
            !payload.contains_key("ready"),
            "a tombstone must not carry `ready`: {payload:?}"
        );
    }

    /// A `tombstone: null` is not a tombstone. Getting this wrong would send
    /// a live task down the tombstone arm and strip its required `ready`.
    #[test]
    fn explicit_null_tombstone_takes_the_live_arm() {
        let mut payload = map(json!({ "key": "k", "tombstone": null }));
        normalize_task_privilege_fields(&mut payload);
        assert_eq!(payload["ready"], json!(false));
        assert!(!payload.contains_key("tombstoned_by"));
    }

    /// An illegal `tombstoned_by` on a live block is left for the validator
    /// to reject, not silently repaired.
    #[test]
    fn illegal_tombstoned_by_on_live_block_is_left_alone() {
        let mut payload = map(json!({
            "key": "k",
            "tombstoned_by": "user",
        }));
        normalize_task_privilege_fields(&mut payload);
        assert_eq!(
            payload["tombstoned_by"],
            json!("user"),
            "fail closed downstream, do not repair"
        );
    }

    /// `refs` is track-scoped: hoisting the recipe's drop into the shared function would delete
    /// the references fork had just rewritten.
    #[test]
    fn refs_are_not_this_functions_business() {
        let reference = calm_types::report_links::format_track_destination("t1", Some("b_1f3a"));
        let mut payload = map(json!({
            "key": "k",
            "goal": "g",
            "refs": [reference],
            "cwd": "/repo",
            "declared_by": "user",
            "ready": true,
        }));
        normalize_task_privilege_fields(&mut payload);
        assert_eq!(
            payload["refs"],
            json!([reference]),
            "fork's rewritten references must survive the shared normalization: {payload:?}"
        );
        assert_eq!(payload["cwd"], json!("/repo"));
    }
}
