//! The three privilege fields on a task block, and the one normalization
//! that must run whenever a task block crosses from one track's authorship
//! into a fresh one.
//!
//! # Why this is a function and not a rule written down twice
//!
//! Two paths copy task blocks into a track that did not author them:
//!
//!   * **fork** — `routes::tracks::prepare_fork_report`, copying a live
//!     track's report into a new track;
//!   * **recipes** (#1292) — saving a report into a `track_recipes` row, and
//!     instantiating one back out.
//!
//! The #1292 design first tried to keep them honest with a meta-test
//! asserting the two paths normalize *the same field set*. That equality is
//! not expressible: fork also rewrites `neige://wave/...` links (it has a
//! source wave to rewrite against; a recipe does not) and **keeps**
//! tombstones (they are that track's audit history), while a recipe **drops**
//! them (a tombstone blocks re-declaring its key, so carrying one into a
//! recipe would poison every wave instantiated from it). The sets can never
//! be equal, so the equation could only ever have been satisfied by
//! weakening one side.
//!
//! What the two paths genuinely share is exactly this: the per-block
//! privilege-field normalization below. Sharing it as a function makes
//! "these two agree" a constructional fact rather than a claim needing an
//! oracle. Link rewriting and tombstone policy stay where they differ, and
//! are tested there.

use serde_json::{Map, Value};

/// Normalize the three privilege fields on one task-block payload so the
/// block carries no authority inherited from the track it came from.
///
/// The fields, and why each is a privilege:
///
///   * **`declared_by`** — `track_report_edit_guard::guard_task_declarations`
///     treats a `"user"` declaration as user-owned, which no spec author may
///     edit or delete. A copied block claiming user authorship would be a
///     block the new track's spec can never touch.
///   * **`tombstoned_by`** — the second attribution field, immutable once a
///     block is a tombstone, and equally load-bearing for user-ownership.
///   * **`released_by_user`** — answers "did a HUMAN approve this task in
///     THIS wave". Copying a `true` hands the new wave a standing exemption
///     from a decision its user never made, and the UI then hides the
///     "Allow this task" button (`report-blocks/task.tsx`) because the flag
///     is already set — so she cannot even see what to undo.
///
/// # Two deliberate asymmetries
///
/// **The tombstone arm removes `ready` rather than setting it false**, and
/// the live arm does the opposite: `kinds.rs` makes `ready` *required* on a
/// live task and forbids it on a tombstone.
///
/// **`released_by_user` is removed rather than written as an explicit
/// `false`.** Absent and `false` are identical to every reader
/// (`.unwrap_or(false)`), but *not* to
/// `track_report_edit_guard`, which compares the raw `Option<&Value>` and
/// rejects any non-user edit that changes it. Absent is the shape a fresh
/// declaration has, byte for byte; writing an explicit `false` would make
/// these the only blocks a spec author must echo the field back on.
///
/// # What this deliberately does not do
///
/// It does not sanitize an *illegal* `tombstoned_by` on a non-tombstone
/// block. That shape is rejected downstream by `validate_payload` ("must be
/// absent from a non-tombstone task"), and a corrupt source should fail
/// closed rather than be silently repaired into a shape it never validly
/// had. Quietly fixing malformed input is how a validator stops being able
/// to tell you anything.
pub(crate) fn normalize_task_privilege_fields(payload: &mut Map<String, Value>) {
    let tombstone = payload
        .get("tombstone")
        .is_some_and(|value| !value.is_null());
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
        // Absent, not `false` — see the doc comment: the guard compares the
        // raw Option and a fresh declaration has no key here.
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
}
