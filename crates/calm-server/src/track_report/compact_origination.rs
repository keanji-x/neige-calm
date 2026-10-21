//! Compact task origination policy shared by edit and structural report writes.
use super::task_changes::{TaskBlockChange, task_block_changes};
use super::*;

/// Runs after the op has produced its final diff, before the general attribution guard.
/// Compact origination is forbidden even when a non-Planner claims `declared_by: "spec"`.
pub(super) fn guard_compact_origination(
    before_blocks: &[ReportBlock],
    after_blocks: &[ReportBlock],
    author: EditAuthor,
) -> Result<(), CalmError> {
    if author != EditAuthor::Planner {
        for block in task_block_changes(before_blocks, after_blocks)
            .into_iter()
            .filter_map(TaskBlockChange::authored_live)
        {
            if block
                .payload
                .get("kind")
                .and_then(serde_json::Value::as_str)
                == Some("compact")
                && !before_blocks.iter().any(|old| {
                    old.id == block.id
                        && old.kind == calm_types::report_blocks::KIND_TASK
                        && calm_types::report_blocks::tasks::task_block_is_live(old)
                        && old.payload.get("kind").and_then(serde_json::Value::as_str)
                            == Some("compact")
                })
            {
                return Err(CalmError::Forbidden(
                    "Only the Planner may originate a compact task.".into(),
                ));
            }
        }
    }
    Ok(())
}
