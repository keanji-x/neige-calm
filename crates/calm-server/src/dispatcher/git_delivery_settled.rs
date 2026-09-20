//! Planner wake for `task.git_delivery_settled` (#1727 S4 slice 2 PR-A).
//!
//! The event carries the settlement; the observation adds the two things only rows know: the
//! task `key` the Planner addresses (looked up by `task_id`, like `task.gate_result`) and the
//! lease worktree path while it still exists (`retained_path`, `None` once removed). Live push
//! and boot catch-up share this one mapping. No tasks row, or a row on another track, means no
//! observation — the same outcome the other row-backed settlement mappings produce.
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::Result;
use crate::event::Event;
use crate::harness::Observation;
use crate::ids::TrackId;

pub(crate) async fn observation(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    event: &Event,
) -> Result<Option<Observation>> {
    let Event::TaskGitDeliverySettled {
        task_id,
        track_id: event_track_id,
        result,
        ..
    } = event
    else {
        return Ok(None);
    };
    if event_track_id != track_id {
        return Ok(None);
    }
    let track_id = track_id.clone();
    let task_id = task_id.clone();
    let result = result.clone();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let Some(task) = crate::db::sqlite::task_get_tx(tx, &task_id).await? else {
                return Ok(None);
            };
            if task.track_id != track_id.as_str() {
                return Ok(None);
            }
            let retained_path = match task.worker_card_id.as_deref() {
                Some(worker_card_id) => {
                    crate::operation::workspace_lease::facts::worker_worktree_facts_tx(
                        tx,
                        worker_card_id,
                    )
                    .await?
                    .and_then(|facts| facts.path)
                }
                None => None,
            };
            Ok(Some(Observation::TaskGitDeliverySettled {
                key: task.key,
                attempt_id: task_id,
                result,
                retained_path,
            }))
        })
    })
    .await
}
