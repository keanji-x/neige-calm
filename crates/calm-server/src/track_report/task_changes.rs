//! The task declaration blocks one report write adds, changes or removes: the one comparison the
//! write-time task checks share.

use std::collections::HashMap;

use calm_types::report_blocks::KIND_TASK;
use calm_types::report_blocks::tasks::task_block_is_live;
use calm_types::track_report::ReportBlock;

/// How one task block differs between the document before a write and after it. Blocks match by
/// id and compare by payload; order and `rev` are not compared. A block that is a task on one side
/// only (new, deleted, or of another kind on the other side) is added or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskBlockChange<'a> {
    Added(&'a ReportBlock),
    Changed(&'a ReportBlock),
    Removed(&'a ReportBlock),
}

impl<'a> TaskBlockChange<'a> {
    /// The block as this write authored it: added or changed and still live. Tombstones and
    /// removals carry no declaration content to check.
    pub(crate) fn authored_live(self) -> Option<&'a ReportBlock> {
        match self {
            Self::Added(block) | Self::Changed(block) => task_block_is_live(block).then_some(block),
            Self::Removed(_) => None,
        }
    }
}

/// Every task block change, additions and changes in `after`'s order, then removals in
/// `before`'s order.
pub(crate) fn task_block_changes<'a>(
    before: &'a [ReportBlock],
    after: &'a [ReportBlock],
) -> Vec<TaskBlockChange<'a>> {
    fn tasks_by_id(blocks: &[ReportBlock]) -> HashMap<&str, &ReportBlock> {
        blocks
            .iter()
            .filter(|block| block.kind == KIND_TASK)
            .map(|block| (block.id.as_str(), block))
            .collect()
    }
    let before_tasks = tasks_by_id(before);
    let after_tasks = tasks_by_id(after);
    let written = after
        .iter()
        .filter(|block| block.kind == KIND_TASK)
        .filter_map(|block| match before_tasks.get(block.id.as_str()) {
            None => Some(TaskBlockChange::Added(block)),
            Some(old) if old.payload != block.payload => Some(TaskBlockChange::Changed(block)),
            Some(_) => None,
        });
    let removed = before
        .iter()
        .filter(|block| block.kind == KIND_TASK && !after_tasks.contains_key(block.id.as_str()))
        .map(TaskBlockChange::Removed);
    written.chain(removed).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn block(id: &str, kind: &str, rev: u32, payload: Value) -> ReportBlock {
        ReportBlock {
            id: id.into(),
            kind: kind.into(),
            rev,
            payload,
        }
    }

    fn live(goal: &str) -> Value {
        json!({"key": "k", "kind": "codex", "goal": goal, "declared_by": "user", "ready": true})
    }

    #[test]
    fn task_block_changes_match_by_id_and_payload_across_kinds_and_tombstones() {
        let tombstone = json!({"key": "t", "tombstone": {"reason": null},
            "declared_by": "user", "tombstoned_by": "user"});
        let before = vec![
            block("same", KIND_TASK, 1, live("same")),
            block("edited", KIND_TASK, 1, live("old")),
            block("gone", KIND_TASK, 1, live("gone")),
            block("was-prose", "prose", 1, json!({"markdown": "x"})),
            block("retired", KIND_TASK, 1, live("retired")),
        ];
        let after = vec![
            block("retired", KIND_TASK, 2, tombstone),
            block("was-prose", KIND_TASK, 2, live("now a task")),
            block("edited", KIND_TASK, 2, live("new")),
            // Moved and re-stamped, same payload: unchanged.
            block("same", KIND_TASK, 9, live("same")),
            block("fresh", KIND_TASK, 1, live("fresh")),
        ];
        let changes = task_block_changes(&before, &after);
        assert_eq!(
            changes,
            vec![
                TaskBlockChange::Changed(&after[0]),
                TaskBlockChange::Added(&after[1]),
                TaskBlockChange::Changed(&after[2]),
                TaskBlockChange::Added(&after[4]),
                TaskBlockChange::Removed(&before[2]),
            ]
        );
        let authored: Vec<_> = changes
            .into_iter()
            .filter_map(TaskBlockChange::authored_live)
            .map(|block| block.id.as_str())
            .collect();
        assert_eq!(authored, vec!["was-prose", "edited", "fresh"]);
    }
}
