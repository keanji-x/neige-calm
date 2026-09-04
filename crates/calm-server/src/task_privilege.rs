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
//! not expressible, and the reason is a rule rather than a list.
//!
//! **The rule.** Sort what a task block carries by one question: does the
//! value still mean the same thing in a track that did not author it?
//!
//!   * If yes, it is a claim about authority rather than about a track — the
//!     three privilege fields. Both paths owe it the same treatment for the
//!     same reason, so it is normalized once, by the function below.
//!   * If no — the value names something that only exists relative to a
//!     track — then the two paths *cannot* agree, because fork has a source
//!     track to map the name onto and a recipe has none. Fork carries or
//!     rewrites; a recipe mints fresh or withdraws. A value in this class
//!     cannot live in the function below at all — the function has no way to
//!     know which side called it — so each path answers it at its own
//!     boundary.
//!
//! Instances of the second class, as illustrations rather than a closed
//! list — the rule is what decides a new field, not this enumeration:
//!
//!   * **Block ids and revs** — fork writes the source snapshot through
//!     `ReportDoc::from_blocks_exact`, which writes each id and rev exactly
//!     as supplied. A recipe reaches `ReportDoc::from_payload` with a payload
//!     built by `TrackReportPayload::new`, whose `blocks` is `None`, so the
//!     `reassign_ids` inside it aligns against an empty old-block set: every
//!     block takes the unmatched branch, minting a new id at rev 1.
//!   * **Report links in prose and in `goal`/`acceptance`** — fork rewrites
//!     them onto the copy (`report_links::rewrite_track_links`); a recipe
//!     leaves them alone, having nothing to rewrite them to.
//!   * **`refs`** — fork **rewrites** each entry through
//!     `report_links::rewrite_track_destination`, so a reference to a copied
//!     block follows the copy. A recipe **drops** the field
//!     (`routes::track_recipes::normalize_recipe_body`); the argument for
//!     that, and for why the report links in the previous bullet are kept
//!     even though they reach the same consumers, is in that function's doc.
//!   * **Tombstones** — fork **keeps** them (they are that track's audit
//!     history), a recipe **drops** them (a tombstone blocks re-declaring
//!     its key, so carrying one into a recipe would poison every track
//!     instantiated from it).
//!
//! So the two field sets can never be equal, and the equation could only
//! ever have been satisfied by weakening one side.
//!
//! What the two paths genuinely share is exactly this: the per-block
//! privilege-field normalization below. Sharing it as a function makes
//! "these two agree" a constructional fact rather than a claim needing an
//! oracle. In particular, dropping `refs` must **not** be hoisted into that
//! function: it belongs to the second class, and hoisting it would silently
//! delete the references fork had just rewritten.

use serde_json::{Map, Value};

/// Normalize the three privilege fields on one task-block payload so the
/// block carries no authority inherited from the track it came from.
///
/// The fields, and why each is a privilege:
///
///   * **`declared_by`** — `track_report_edit_guard::guard_task_declarations`
///     treats a `"user"` declaration as user-owned, which no planner agent may
///     edit or delete. A copied block claiming user authorship would be a
///     block the new track's planner can never touch.
///   * **`tombstoned_by`** — the second attribution field, immutable once a
///     block is a tombstone, and equally load-bearing for user-ownership.
///   * **`released_by_user`** — answers "did a HUMAN approve this task in
///     THIS track". Copying a `true` hands the new track a standing exemption
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
/// these the only blocks a planner agent must echo the field back on.
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
    // `"spec"` is FROZEN document vocabulary, not the kernel's actor name.
    // #1316 S3 renamed the actor to Planner everywhere the kernel owns it
    // (`cards.role`, `events.actor`, `EditAuthor`), but `declared_by` /
    // `tombstoned_by` are written by agents into stored report blocks and
    // projected back out of them, so rewriting them would mean rewriting
    // `cards.payload` and `cards.body_crdt` in lockstep — see migration
    // 0083's header. `report_blocks::kinds::validate_declared_by` accepts
    // only `"spec" | "user"`, and `track_report_edit_guard::author_name` maps
    // `EditAuthor::Planner` onto this same string; changing it here alone
    // deadlocks the two guards against each other.
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

    /// The shared middle must not touch `refs`.
    ///
    /// `refs` is on the track-scoped side of the module doc's rule: the
    /// recipe write boundary drops the field, fork rewrites it onto the
    /// copy, and neither belongs here. Hoisting the drop down here —
    /// the refactor the two `remove` sites invite — would delete the
    /// references fork had just rewritten, and fork's own end-to-end test
    /// (`track_report_fork::forked_task_refs_are_rewritten_onto_the_copy_and_resolve`)
    /// is in a different target than this one. Both spellings of the
    /// asymmetry are checked, so a gate that builds only one still catches
    /// it.
    #[test]
    fn refs_are_not_this_functions_business() {
        // Built by the production formatter rather than spelled out: a
        // literal here would be this test's own idea of what a reference
        // looks like.
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
