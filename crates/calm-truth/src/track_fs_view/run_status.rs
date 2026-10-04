//! The one run-status derivation behind every runs view: the live `neige.track.{ls,cat}`
//! projection and the track VCS snapshot and delta (#2058).

use super::RunEventProjection;
use crate::db::sqlite::CanceledTaskAttempt;
use crate::track_fs_dto::TrackFsRunStatus;

/// The worker's latest self-report for a run.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FinalRunEvent<'a> {
    Completed(&'a RunEventProjection),
    Failed(&'a RunEventProjection),
}

/// The later of the two self-reports; a failure wins a tie.
pub(crate) fn latest_final_event<'a>(
    completed: Option<&'a RunEventProjection>,
    failed: Option<&'a RunEventProjection>,
) -> Option<FinalRunEvent<'a>> {
    match (completed, failed) {
        (Some(done), Some(fail)) if done.event_id > fail.event_id => {
            Some(FinalRunEvent::Completed(done))
        }
        (_, Some(fail)) => Some(FinalRunEvent::Failed(fail)),
        (Some(done), None) => Some(FinalRunEvent::Completed(done)),
        (None, None) => None,
    }
}

/// A worker self-report decides the status. Without one, a canceled task row ends the run: a
/// cancel moves the row and emits no `task.*` event for the attempt, and its worker card stays.
pub(crate) fn run_status(
    requested: Option<&RunEventProjection>,
    completed: Option<&RunEventProjection>,
    failed: Option<&RunEventProjection>,
    canceled: Option<&CanceledTaskAttempt>,
    has_worker_card: bool,
) -> (TrackFsRunStatus, Option<i64>) {
    if requested.is_none() {
        return (TrackFsRunStatus::Unknown, None);
    }
    match (latest_final_event(completed, failed), canceled) {
        (Some(FinalRunEvent::Completed(event)), _) => (TrackFsRunStatus::Completed, Some(event.at)),
        (Some(FinalRunEvent::Failed(event)), _) => (TrackFsRunStatus::Failed, Some(event.at)),
        (None, Some(canceled)) => (TrackFsRunStatus::Canceled, canceled.finished_at_ms),
        (None, None) if has_worker_card => (TrackFsRunStatus::Running, None),
        (None, None) => (TrackFsRunStatus::Requested, None),
    }
}
