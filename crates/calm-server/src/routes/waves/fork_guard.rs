//! The single production entry that may skip task-declaration Rule 1 for a fork.

use calm_types::event::EditAuthor;
use calm_types::report_blocks::KIND_TASK;
use calm_types::wave_report::ReportBlock;
use serde_json::Value;

use crate::error::CalmError;

/// Fork copies an existing snapshot into a new report, so copied task
/// attribution is not a fresh declaration by the wave creator. This is the
/// module's sole exported exemption semantic.
pub(in crate::routes::waves) fn guard_forked_blocks(
    after: &[ReportBlock],
    author: EditAuthor,
) -> Result<(), CalmError> {
    guard_forked_blocks_impl(after, author)
}

fn guard_forked_blocks_impl(after: &[ReportBlock], author: EditAuthor) -> Result<(), CalmError> {
    // With an empty `before`, the ordinary guard's only post-Rule-1
    // constraint is Rule 5: non-users cannot assert user release.
    for block in after.iter().filter(|block| block.kind == KIND_TASK) {
        if author != EditAuthor::User
            && block
                .payload
                .get("released_by_user")
                .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(CalmError::BadRequest(
                "released_by_user may only be set by a user".into(),
            ));
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

    #[test]
    fn fork_guard_exempts_rule_one_but_still_enforces_rule_five() {
        let copied = task(json!({"key": "build", "declared_by": "spec"}));
        guard_forked_blocks(std::slice::from_ref(&copied), EditAuthor::User).unwrap();

        let mut released = copied;
        released.payload["released_by_user"] = Value::Bool(true);
        let error = guard_forked_blocks(&[released], EditAuthor::Spec).unwrap_err();
        assert!(error.to_string().contains("released_by_user"));
    }
}
