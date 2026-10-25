//! Failure-only projections: never print a frame, task, grant or native payload.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum PhaseWaitStage {
    Unspecified,
    UserStart,
    UserComplete,
    ExpiredMaintenance,
}

impl Fixture {
    pub(super) async fn wait_phase_at(
        &self,
        phase: HarnessPhaseTag,
        stage: PhaseWaitStage,
    ) -> CanonicalHarnessSnapshot {
        let mut last = None;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = self.frame().await;
                if frame.header.phase == phase {
                    return frame;
                }
                last = Some(frame);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        match result {
            Ok(frame) => frame,
            Err(_) => {
                // These observations happen AFTER the failed deadline. They do not
                // extend the wait, turn a timeout into success, or certify END.
                let inner = &self.harness.inner;
                let task_id = format!("{}:compact", inner.track_id);
                let task = match tokio::time::timeout(
                    Duration::from_millis(250),
                    self.original.repo.task_get(&task_id),
                )
                .await
                {
                    Ok(Ok(Some(task))) => json!({"id":task.id,"status":task.status}),
                    Ok(Ok(None)) => json!({"id":task_id,"state":"absent"}),
                    Ok(Err(_)) => json!({"id":task_id,"state":"read_error"}),
                    Err(_) => json!({"id":task_id,"state":"observation_timeout"}),
                };
                let persisted = last.as_ref().map(|frame| {
                    let header = &frame.header;
                    json!({
                        "phase":header.phase,
                        "thread_id":header.last_thread_id,
                        "turn_id":header.last_turn_id,
                        "wedged":header.wedged_reason.is_some(),
                        "compact":header.compact_outcome.as_ref().map(|outcome| json!({
                            "operation_id":outcome.operation_id,
                            "thread_id":outcome.thread_id,
                            "turn_id":outcome.turn_id,
                            "status":outcome.status,
                        })),
                    })
                });
                let native = last
                    .as_ref()
                    .and_then(|frame| frame.header.last_thread_id.as_deref())
                    .map(|thread| self.daemon.native_diagnostics_for_test(thread));
                panic!(
                    "phase wait timed out: stage={stage:?} expected={phase:?} \
                     current_last_persisted={:?} last_persisted={persisted:?} \
                     task_after_timeout={task} native_after_timeout={native:?}",
                    last.as_ref().map(|frame| frame.header.phase),
                );
            }
        }
    }
}
