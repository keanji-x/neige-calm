//! The single production entry that may skip task-declaration Rule 1 for a
//! fork, and — since #1115 — the fail-closed belt over the fork's own
//! `released_by_user` normalization.

use calm_types::report_blocks::KIND_TASK;
use calm_types::wave_report::ReportBlock;
use serde_json::Value;

use crate::error::CalmError;

/// Fork copies an existing snapshot into a new report, so copied task
/// attribution is not a fresh declaration by the wave creator. This is the
/// module's sole exported exemption semantic.
///
/// It is also a fail-closed **belt** over the release normalization in
/// `super::prepare_fork_report`: no forked task block may reach a new wave
/// carrying `released_by_user`, no matter who forks it. The check takes no
/// author on purpose — see [`guard_forked_blocks_impl`].
pub(in crate::routes::waves) fn guard_forked_blocks(
    after: &[ReportBlock],
) -> Result<(), CalmError> {
    guard_forked_blocks_impl(after)
}

fn guard_forked_blocks_impl(after: &[ReportBlock]) -> Result<(), CalmError> {
    // With an empty `before`, the ordinary guard's only post-Rule-1 constraint
    // is Rule 5 (`released_by_user` is user-only). What runs here is strictly
    // stronger than §3.7 Rule 5: it holds for **every** author, so no task
    // block may be forked with the flag set at all.
    //
    // #1115 — this is a belt over the normalization in
    // `super::prepare_fork_report`, not a restatement of Rule 5.
    //
    // * **What it belts**: the live-task arm of `prepare_fork_report`
    //   (`routes/waves.rs`, the `payload.remove("released_by_user")` next to
    //   `ready: false`), plus the tombstone arm's reliance on
    //   `validate_payload` (`report_blocks/kinds.rs`: any non-`{key, tombstone,
    //   declared_by, tombstoned_by}` field on a tombstone is rejected). Both run
    //   over every block *before* `guard_forked_blocks` is called, so on the
    //   normal path this loop finds nothing. **That no-op is the intended
    //   steady state, not evidence the rule is vacuous.**
    //
    // * **What it durably buys, and who reads the value it protects**: the flag
    //   is read by `calm-truth/src/db/sqlite/task_projection.rs` (a set flag
    //   suppresses the `declare_and_wait` diagnostic, and `schedulable` follows
    //   from the diagnostics being empty) and by
    //   `web/src/pages/report-blocks/task.tsx` (a set flag hides the "Allow this
    //   task" button). If the normalization is ever deleted, moved after this
    //   call, or narrowed to a subset of blocks, a template's release would
    //   reach a wave whose user approved nothing — and she would have no button
    //   left to notice it with. This loop turns that regression into an
    //   immediate 400 on wave creation instead of a silent privilege grant.
    //
    // * **Why no author parameter**: the pre-#1115 shape gated this on
    //   `author != EditAuthor::User`, which made it a no-op for the only case
    //   that mattered — a browser fork sends no `X-Calm-Actor` header, so the
    //   wave-create handler classified it as a `User` edit. The author is no
    //   longer threaded into the fork guard at all, so that gate cannot be
    //   reintroduced without re-plumbing it through `prepare_fork_report`.
    for block in after.iter().filter(|block| block.kind == KIND_TASK) {
        if block
            .payload
            .get("released_by_user")
            .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(CalmError::BadRequest(format!(
                "wave create: forked task block {} still carries released_by_user; \
                 a fork may not import any wave's user release",
                block.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn task(payload: Value) -> ReportBlock {
        ReportBlock {
            id: "b_0001".into(),
            kind: KIND_TASK.into(),
            rev: 1,
            payload,
        }
    }

    /// Rule 1 is exempted (a copied `declared_by` is not a fresh declaration),
    /// while the release belt bites. #1115: it bites for the **user** author
    /// too — that is the whole difference from §3.7 Rule 5, and the case a
    /// browser fork actually takes.
    #[test]
    fn fork_guard_exempts_rule_one_but_belts_release_for_every_author() {
        let copied = task(json!({"key": "build", "declared_by": "spec"}));
        guard_forked_blocks(std::slice::from_ref(&copied)).unwrap();

        let mut absent_is_fine = copied.clone();
        absent_is_fine.payload["released_by_user"] = Value::Bool(false);
        guard_forked_blocks(&[absent_is_fine]).unwrap();

        let mut released = copied;
        released.payload["released_by_user"] = Value::Bool(true);
        let error = guard_forked_blocks(&[released]).unwrap_err();
        assert!(error.to_string().contains("released_by_user"));
    }
}
