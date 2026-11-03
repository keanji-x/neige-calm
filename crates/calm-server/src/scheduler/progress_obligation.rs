//! Child reporting policies over the shared durable obligation primitive.
use super::*;
use crate::db::sqlite::progress_obligation::progress_obligations_due_tx;
use crate::db::sqlite::task_child_failure::{
    CHILD_PLANNER_FAILURE_PURPOSE, task_claim_child_failure_notices_tx,
    task_reconcile_child_failure_obligations_tx,
};
use crate::db::sqlite::task_progress::{
    CHILD_BOOTSTRAP_PURPOSE, task_claim_child_bootstrap_notice_tx,
    task_reconcile_child_bootstrap_obligations_tx,
};

impl Scheduler {
    /// No spawn permits, start fences, RPCs or context-recovery gates. This watchdog
    /// must be runnable while the dispatch it supervises is blocked.
    pub async fn sweep_progress_obligations(&self) -> Result<()> {
        // Enrollment repair/release is internal maintenance and may produce no events.
        // Commit it through the no-event transaction contract, then atomically commit
        // any actual notification together with its pending -> notified CAS.
        crate::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                task_reconcile_child_bootstrap_obligations_tx(tx).await?;
                task_reconcile_child_failure_obligations_tx(tx).await?;
                Ok(())
            })
        })
        .await?;
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let now = now_ms();
                    let due = progress_obligations_due_tx(tx, CHILD_BOOTSTRAP_PURPOSE, now).await?;
                    let mut events = Vec::new();
                    for obligation in due {
                        let Some(target) =
                            task_claim_child_bootstrap_notice_tx(tx, &obligation, now).await?
                        else {
                            continue;
                        };
                        let track = target.track_id;
                        let area = target.area_id;
                        let task_key = target.task_key;
                        let track = TrackId::from(track);
                        events.push((
                            ActorId::KernelDispatcher,
                            EventScope::Track {
                                track: track.clone(),
                                area: area.into(),
                            },
                            Event::TrackWakeRequested {
                                track_id: track,
                                source: "progress-obligation".into(),
                                key: format!("{}:{}", obligation.execution_id, obligation.purpose),
                                text: format!(
                                    include_str!("../../prompts/child-bootstrap-overdue.md"),
                                    task_key = task_key,
                                    attempt_id = obligation.execution_id
                                ),
                            },
                        ));
                    }
                    for target in task_claim_child_failure_notices_tx(tx, now).await? {
                        let track = TrackId::from(target.track_id);
                        events.push((
                            ActorId::KernelDispatcher,
                            EventScope::Track {
                                track: track.clone(),
                                area: target.area_id.into(),
                            },
                            Event::TrackWakeRequested {
                                track_id: track,
                                source: "progress-obligation".into(),
                                key: format!(
                                    "{}:{}",
                                    target.execution_id, CHILD_PLANNER_FAILURE_PURPOSE
                                ),
                                text: format!(
                                    include_str!("../../prompts/child-planner-failed.md"),
                                    observed_at_ms = now,
                                    task_key = target.task_key,
                                    attempt_id = target.task_id,
                                    child_track_id = target.child_track_id,
                                    worker_session_id = target.worker_session_id,
                                    failure_epoch = target.failure_epoch,
                                ),
                            },
                        ));
                    }
                    if events.is_empty() {
                        return Err(race_lost_err());
                    }
                    Ok(((), events))
                })
            },
        )
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_race_lost(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn pending_progress_notices(
        &self,
    ) -> Result<Vec<crate::db::sqlite::task_progress::ParentProgressNotice>> {
        let db = self
            .repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("progress obligations require SQLite".into()))?;
        let mut notices =
            crate::db::sqlite::task_progress::task_pending_child_bootstrap_notices(&db).await?;
        notices.extend(
            crate::db::sqlite::task_child_failure::task_pending_child_failure_notices(&db).await?,
        );
        notices.sort_unstable_by_key(|notice| notice.event_id);
        Ok(notices)
    }
}
