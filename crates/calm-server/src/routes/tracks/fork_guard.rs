//! The fork's task-declaration exemption and the fail-closed belt over its
//! `released_by_user` normalization.

use calm_types::report_blocks::KIND_TASK;
use calm_types::track_report::ReportBlock;
use serde_json::Value;

use crate::error::CalmError;

/// Copied task attribution is not a fresh declaration by the track creator. No forked
/// task block may carry a `released_by_user` other than `false`, whoever forks it.
pub(in crate::routes::tracks) fn guard_forked_blocks(
    after: &[ReportBlock],
) -> Result<(), CalmError> {
    guard_forked_blocks_impl(after)
}

fn guard_forked_blocks_impl(after: &[ReportBlock]) -> Result<(), CalmError> {
    // Strictly stronger than Rule 5: it holds for every author. `!= Bool(false)` so an
    // absent key and an explicit `false` both pass, while `null` or a string fail closed.
    // On the normal path `prepare_fork_report` has already normalized, so this finds nothing.
    for block in after.iter().filter(|block| block.kind == KIND_TASK) {
        if block
            .payload
            .get("released_by_user")
            .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(CalmError::BadRequest(format!(
                "track create: forked task block {} still carries released_by_user; \
                 a fork may not import any track's user release",
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

    /// Both the absent key and an explicit `false` are accepted on purpose: the belt catches
    /// a normalization regression, not the encoding of "not released".
    #[test]
    fn fork_guard_exempts_rule_one_but_belts_release_for_every_author() {
        let copied = task(json!({"key": "build", "declared_by": "spec"}));
        assert!(copied.payload.get("released_by_user").is_none());
        guard_forked_blocks(std::slice::from_ref(&copied)).unwrap();

        let mut explicit_false_is_fine = copied.clone();
        explicit_false_is_fine.payload["released_by_user"] = Value::Bool(false);
        guard_forked_blocks(&[explicit_false_is_fine]).unwrap();

        let mut released = copied;
        released.payload["released_by_user"] = Value::Bool(true);
        let error = guard_forked_blocks(&[released]).unwrap_err();
        assert!(error.to_string().contains("released_by_user"));
    }
}
