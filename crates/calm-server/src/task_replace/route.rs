//! The one dispatch-time recheck of a replacement successor (design §4.4): the Planner may edit
//! the successor's block like any declaration, and an edit can move it off the route the
//! replacement admitted. `drive_spawn` asks here first, before the child-Track branch.

use calm_types::task_execution::IsolatedCodexSelection;
use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;
use serde_json::Value;

use super::receipt;
use super::refusal::Refusal;
use crate::error::Result;
use crate::model::{Task, TaskKind, Track, TrackWorkspaceKind};
use crate::operation::Tx;

/// Whether `task` is a replacement successor (its key has a receipt on its Track).
pub(crate) async fn is_successor_tx(tx: &mut Tx<'_>, task: &Task) -> Result<bool> {
    Ok(receipt::by_successor_tx(tx, &task.track_id, &task.key)
        .await?
        .is_some())
}

/// The replaceable route (design §4.1): an attached Track, a codex or claude task running inside
/// it, no isolated selector. A malformed selector is off the route, not an error. The one
/// predicate both the replacement's admission (on the successor declaration it builds) and
/// `drive_spawn` (on the successor row it dispatches) apply.
pub(crate) fn route_is_replaceable(
    workspace: TrackWorkspaceKind,
    kind: &str,
    spawn: &str,
    context: &Value,
) -> bool {
    workspace == TrackWorkspaceKind::Attached
        && matches!(kind, "codex" | "claude")
        && spawn == TASK_IN_TRACK_ROUTE
        && matches!(IsolatedCodexSelection::from_context(context), Ok(None))
}

/// [`route_is_replaceable`] on a task row.
pub(crate) fn on_route(task: &Task, track: &Track) -> bool {
    let kind = match task.kind {
        TaskKind::Codex => "codex",
        TaskKind::Claude => "claude",
        TaskKind::Terminal => "terminal",
    };
    // Unparsable context is a malformed selector: off the route.
    serde_json::from_str::<Value>(&task.context_json).is_ok_and(|context| {
        route_is_replaceable(track.workspace.kind, kind, &task.spawn, &context)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_or_isolated_context_is_off_the_route() {
        let on = |context: &Value| {
            route_is_replaceable(
                TrackWorkspaceKind::Attached,
                "codex",
                TASK_IN_TRACK_ROUTE,
                context,
            )
        };
        assert!(on(&serde_json::json!({})));
        assert!(!on(
            &serde_json::json!({"neige_execution": {"version": "bogus"}})
        ));
        assert!(!on(&serde_json::json!({"neige_execution": {
            "version": "isolated-codex-v1", "workspace": "empty"}})));
    }
}

/// The spawn-failure reason of a successor edited off the route (`spawn-failed: <this>`).
pub(crate) fn route_changed_reason() -> String {
    format!("refused: {}", Refusal::RouteChanged.message(""))
}
