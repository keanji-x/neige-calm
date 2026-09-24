//! The one dispatch-time recheck of a replacement successor (design §4.4): the Planner may edit
//! the successor's block like any declaration, and an edit can move it off the route the
//! replacement admitted. `drive_spawn` asks here first, before the child-Track branch.

use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;

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
/// it, no isolated selector. A malformed selector is off the route, not an error.
pub(crate) fn on_route(task: &Task, track: &Track) -> bool {
    track.workspace.kind == TrackWorkspaceKind::Attached
        && matches!(task.kind, TaskKind::Codex | TaskKind::Claude)
        && task.spawn == TASK_IN_TRACK_ROUTE
        && matches!(crate::isolated_codex::selected(task), Ok(false))
}

/// The spawn-failure reason of a successor edited off the route (`spawn-failed: <this>`).
pub(crate) fn route_changed_reason() -> String {
    format!("refused: {}", Refusal::RouteChanged.message(""))
}
