//! Slice 2 declares compact tasks; the executor connection follows in slice 3.
use super::*;

impl Scheduler {
    pub(super) async fn refuse_compact(&self, task: &Task, track: &Track) -> Result<()> {
        let task = task.clone();
        let track = track.clone();
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let events =
                        fail_worker_task_tx(tx, &task, &track, "compaction-refused", "unavailable")
                            .await?;
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
}
