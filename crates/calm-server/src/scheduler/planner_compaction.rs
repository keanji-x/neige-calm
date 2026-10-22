//! Worker-less maintenance for the declaring track's current Planner session.
use super::*;
use crate::harness::compact_outcome::{CompactionAnswer, CompactionRefusal};

impl Scheduler {
    /// Boot binds this after constructing the scheduler, before scheduling starts.
    pub fn bind_compaction_harness(&self, harness: crate::harness::HarnessRegistry) {
        assert!(
            self.compaction_harness.set(harness).is_ok(),
            "compaction harness already bound"
        );
    }

    pub(super) async fn drive_compact(
        &self,
        task: &Task,
        track: &Track,
        recovering: bool,
    ) -> Result<()> {
        let unavailable_reason = if recovering || task.status == TaskStatus::Running {
            "superseded"
        } else {
            "unavailable"
        };
        let Some(card) =
            crate::planner_session::resolve_card(self.repo.as_ref(), &self.write, &track.id)
                .await?
        else {
            return self.refuse_compact(task, track, unavailable_reason).await;
        };
        let _fence =
            crate::per_card_lock::lock_card(&self.planner_recovery_locks, card.as_str()).await;
        // Re-read after the fence: reset/rewind may have replaced the session while we waited.
        let Some(current_card) =
            crate::planner_session::resolve_card(self.repo.as_ref(), &self.write, &track.id)
                .await?
        else {
            return self.refuse_compact(task, track, "superseded").await;
        };
        // Sweep rows are snapshots taken before semaphore and card-fence waits.
        let Some(current_task) = self.repo.task_get(&task.id).await? else {
            return Ok(());
        };
        let task = &current_task;
        if !matches!(task.status, TaskStatus::Dispatched | TaskStatus::Running) {
            return Ok(());
        }
        if card != current_card {
            return self.refuse_compact(task, track, "superseded").await;
        }
        if crate::conversation_inputs::has_canonical_storage(self.repo.as_ref(), card.as_str())
            .await?
        {
            return self
                .refuse_compact(
                    task,
                    track,
                    if task.status == TaskStatus::Running {
                        "superseded"
                    } else {
                        "unsupported_conversation"
                    },
                )
                .await;
        }
        let Some(session) =
            crate::planner_session::historical_session(self.repo.as_ref(), &card).await?
        else {
            return self.refuse_compact(task, track, unavailable_reason).await;
        };
        let recorded = session
            .handle_state_json
            .as_ref()
            .and_then(|value| value.get("planner_compaction"))
            .and_then(|value| value.get("request_id"))
            .and_then(Value::as_str)
            == Some(task.id.as_str());
        // Persisted intent, rather than registry presence, owns restart recovery.
        if recorded {
            return Ok(());
        }
        if task.status == TaskStatus::Running {
            return self.refuse_compact(task, track, "superseded").await;
        }
        for retry in 0..3 {
            let result = match self
                .compaction_harness
                .get()
                .and_then(|registry| registry.get(&session.id))
            {
                Some(harness) => harness.request_compaction(task.id.clone()).await,
                // A persisted session may still be restoring or waking from hibernation.
                None => Err(CalmError::ServiceUnavailable(
                    "Planner runtime is not installed".into(),
                )),
            };
            let reason = match result {
                Ok(CompactionAnswer::Scheduled) => return Ok(()),
                Ok(CompactionAnswer::AlreadyPending { .. }) => "already_pending",
                Ok(CompactionAnswer::Refused(reason)) => match reason {
                    CompactionRefusal::UnsupportedProvider => "unsupported_provider",
                    CompactionRefusal::UnsupportedConversation => "unsupported_conversation",
                    CompactionRefusal::ConversationUnavailable => "conversation_unavailable",
                    CompactionRefusal::CompactionInProgress => "compaction_in_progress",
                    CompactionRefusal::NotCurrentSession => "not_current_session",
                },
                Err(CalmError::ServiceUnavailable(_)) => {
                    if retry < 2 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            return self.refuse_compact(task, track, reason).await;
        }
        Ok(())
    }

    async fn refuse_compact(&self, task: &Task, track: &Track, reason: &str) -> Result<()> {
        let task = task.clone();
        let track = track.clone();
        let reason = reason.to_owned();
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let events =
                        fail_worker_task_tx(tx, &task, &track, "compaction-refused", &reason)
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
