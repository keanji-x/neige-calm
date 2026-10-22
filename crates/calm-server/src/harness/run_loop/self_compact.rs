//! Planner requests select their runtime maintenance owner at idle admission.
use super::*;
use crate::harness::compact_outcome::{CompactStatus, PlannerCompaction, PlannerCompactionPhase};
use crate::harness::compact_outcome::{CompactionAnswer, CompactionRefusal};

impl PlannerHarness {
    /// Acknowledge maintenance intent for this session. Canonical task rows already
    /// own durable intent; historical sessions persist it under their snapshot guard.
    /// The caller owns authorization and request identity.
    /// The same ID is answered idempotently while its record is retained. Durable
    /// once-per-ID execution belongs to the task layer: settlement terminalizes
    /// the task row before this record can be cleared.
    pub async fn request_compaction(&self, request_id: String) -> Result<CompactionAnswer> {
        // Canonical intent is already durable in the dispatched task row. Its
        // read-only acknowledgement must not queue behind the native RPC or a
        // track fence that the scheduler itself may be waiting to release.
        if self.inner.inputs.canonical().is_some() {
            return request(&self.inner, request_id).await;
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, result) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::RequestCompaction { request_id, answer })
                    .map_err(map_observation_send_error)?;
                result
                    .await
                    .map_err(|_| CalmError::Conflict("conversation unavailable".into()))?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => request(&self.inner, request_id).await,
        }
    }
}

pub(super) async fn request(inner: &Arc<Inner>, request_id: String) -> Result<CompactionAnswer> {
    if !PlannerBackend::manual_compact_supported(&inner.backend.provider()) {
        return Ok(CompactionAnswer::Refused(
            CompactionRefusal::UnsupportedProvider,
        ));
    }
    if inner.shutting_down.load(Ordering::SeqCst)
        || matches!(*inner.state.lock().await, HarnessState::Wedged { .. })
        || crate::conversation_archive::require_unarchived(
            inner.repo.as_ref(),
            inner.card_id.as_str(),
        )
        .await
        .is_err()
    {
        return Ok(CompactionAnswer::Refused(
            CompactionRefusal::ConversationUnavailable,
        ));
    }
    if inner.inputs.canonical().is_some() {
        return compact_command::canonical::planner::request(inner, &request_id).await;
    }
    if let Some(request) = inner.planner_compaction.lock().await.as_ref() {
        if request.request_id == request_id {
            return Ok(CompactionAnswer::Scheduled);
        }
        if request.pending() {
            return Ok(CompactionAnswer::AlreadyPending {
                request_id: request.request_id.clone(),
            });
        }
    }
    if matches!(
        *inner.state.lock().await,
        HarnessState::Compacting { .. } | HarnessState::CompactionRunning { .. }
    ) || inner
        .compact_outcome
        .lock()
        .await
        .as_ref()
        .is_some_and(|outcome| {
            matches!(
                outcome.status,
                CompactStatus::Started | CompactStatus::Unconfirmed
            )
        })
    {
        return Ok(CompactionAnswer::Refused(
            CompactionRefusal::CompactionInProgress,
        ));
    }
    let Some(turn) = inner.last_turn_id.lock().await.clone() else {
        return Ok(CompactionAnswer::Refused(
            CompactionRefusal::ConversationUnavailable,
        ));
    };
    let previous = inner.planner_compaction.lock().await.clone();
    *inner.planner_compaction.lock().await = Some(PlannerCompaction {
        request_id: request_id.clone(),
        requested_turn_id: turn,
        requested_at_ms: crate::model::now_ms(),
        phase: PlannerCompactionPhase::Requested,
        wake_enqueued: false,
    });
    match persist_snapshot_inner(inner, None, None, false).await {
        Ok(true) => Ok(CompactionAnswer::Scheduled),
        result => {
            *inner.planner_compaction.lock().await = previous;
            match result {
                Err(error) => Err(error),
                _ => Ok(CompactionAnswer::Refused(
                    CompactionRefusal::NotCurrentSession,
                )),
            }
        }
    }
}

pub(super) async fn tick(inner: &Arc<Inner>, ingress_empty: bool) -> Result<()> {
    let phase = inner
        .planner_compaction
        .lock()
        .await
        .as_ref()
        .map(|r| r.phase.clone());
    if matches!(phase, Some(PlannerCompactionPhase::Settled { .. })) {
        // Boot turns Submitted into Unconfirmed. Checkpoint the settlement and its
        // wake even when the recovered owner is wedged and cannot issue another turn.
        if inner
            .planner_compaction
            .lock()
            .await
            .as_ref()
            .is_some_and(|r| !r.wake_enqueued)
        {
            persist_snapshot(inner).await?;
        }
        return Ok(());
    }
    // The owner may select a ready tick ahead of an already queued command.
    // Check ingress as well as the folded queue before admitting Planner maintenance.
    if !ingress_empty
        || !matches!(phase, Some(PlannerCompactionPhase::Requested))
        || !matches!(
            *inner.state.lock().await,
            HarnessState::Idle | HarnessState::TurnCompleted { .. }
        )
        || inner
            .compaction_retry_after
            .lock()
            .await
            .is_some_and(|at| Instant::now() < at)
    {
        return Ok(());
    }
    *inner.compaction_retry_after.lock().await = Some(Instant::now() + Duration::from_secs(1));
    let admitted = match compact_command::admit_historical(inner).await {
        Ok(admitted) => admitted,
        // Admission never consumes a request; input and rewind take precedence.
        Err(_) => return Ok(()),
    };
    compact_command::start(inner, admitted, compact_command::Origin::PlannerRequest).await
}

pub(super) async fn clear_settled(inner: &Arc<Inner>) {
    let mut request = inner.planner_compaction.lock().await;
    if request.as_ref().is_some_and(|r| !r.pending()) {
        *request = None;
    }
}

/// Set the marker in the value that will be written with this event. No live
/// mutation occurs until the guarded snapshot transaction commits successfully.
pub(super) fn prepare_wake(
    inner: &Inner,
    snapshot: &mut HarnessSnapshot,
) -> Option<(EventScope, Event)> {
    let request = snapshot.planner_compaction.as_mut()?;
    let PlannerCompactionPhase::Settled { status, .. } = &request.phase else {
        return None;
    };
    if request.wake_enqueued {
        return None;
    }
    let wake = prepare_request_wake(
        &inner.track_id,
        harness_event_scope(inner, "track.wake_requested"),
        &request.request_id,
        status,
    );
    request.wake_enqueued = true;
    Some(wake)
}

pub(super) fn prepare_request_wake(
    track: &TrackId,
    scope: EventScope,
    request_id: &str,
    status: &CompactStatus,
) -> (EventScope, Event) {
    let text = match status {
        CompactStatus::Completed => {
            include_str!("../../../prompts/planner_compaction_completed.md")
                .trim_end()
                .replace("{request_id}", request_id)
        }
        CompactStatus::Started
        | CompactStatus::Interrupted
        | CompactStatus::Failed
        | CompactStatus::Refused
        | CompactStatus::Unconfirmed => {
            include_str!("../../../prompts/planner_compaction_incomplete.md")
                .trim_end()
                .replace("{request_id}", request_id)
                .replace(
                    "{status}",
                    serde_json::to_value(status)
                        .expect("compact status")
                        .as_str()
                        .expect("status string"),
                )
        }
    };
    let event = Event::TrackWakeRequested {
        track_id: track.clone(),
        source: "planner_compaction".into(),
        key: request_id.to_owned(),
        text,
    };
    (scope, event)
}

/// Task facts and the wake share the session snapshot's active-row guard and commit.
pub(super) async fn append_task_lifecycle(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &crate::ids::TrackId,
    request: Option<&PlannerCompaction>,
    scope: &EventScope,
    compact_start: bool,
) -> Result<Vec<crate::event::BroadcastEnvelope>> {
    let Some(request) = request else {
        return Ok(vec![]);
    };
    append_lifecycle(
        tx,
        track,
        &request.request_id,
        &request.phase,
        scope,
        compact_start,
    )
    .await
}

pub(super) async fn append_lifecycle(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &TrackId,
    request_id: &str,
    phase: &PlannerCompactionPhase,
    scope: &EventScope,
    compact_start: bool,
) -> Result<Vec<crate::event::BroadcastEnvelope>> {
    use crate::db::sqlite::{
        TaskReporter, status_detail_with_reason, task_complete_from_worker_tx,
        task_fail_from_worker_tx, task_get_tx, task_mark_compact_running_tx,
    };
    let Some(task) = task_get_tx(tx, request_id).await? else {
        return Ok(vec![]);
    };
    if task.kind != crate::model::TaskKind::Compact || task.track_id != track.as_str() {
        return Err(CalmError::Conflict("compaction task owner mismatch".into()));
    }
    let now = crate::model::now_ms();
    let event = match phase {
        PlannerCompactionPhase::Requested => return Ok(vec![]),
        PlannerCompactionPhase::Submitted { .. } => {
            // Only Start consumes dispatched. Later Submitted checkpoints merely
            // persist observations; a lost Start guard rolls back the whole snapshot.
            if compact_start
                && task_mark_compact_running_tx(tx, &task.id, track.as_str(), now).await? == 0
            {
                return Err(CalmError::Conflict(
                    "compaction task is no longer dispatched".into(),
                ));
            }
            return Ok(vec![]);
        }
        PlannerCompactionPhase::Settled { status, .. } => {
            if *status == CompactStatus::Completed {
                if task_complete_from_worker_tx(
                    tx,
                    &task.id,
                    track.as_str(),
                    TaskReporter::Kernel,
                    now,
                )
                .await?
                    == 0
                {
                    return Ok(vec![]);
                }
                Event::TaskCompleted {
                    idempotency_key: task.id,
                    result: serde_json::Value::Null,
                    artifacts: vec![],
                    agent_message: None,
                }
            } else {
                let status = serde_json::to_value(status)?;
                let detail = status_detail_with_reason(
                    "compaction-incomplete",
                    status.as_str().expect("compact status"),
                );
                if task_fail_from_worker_tx(
                    tx,
                    &task.id,
                    track.as_str(),
                    TaskReporter::Kernel,
                    &detail,
                    now,
                )
                .await?
                    == 0
                {
                    return Ok(vec![]);
                }
                Event::TaskFailed {
                    idempotency_key: task.id,
                    reason: detail,
                    details: None,
                    agent_message: None,
                }
            }
        }
    };
    let id =
        crate::db::sqlite::append_decision_event_in_tx(tx, &ActorId::Kernel, scope, None, &event)
            .await?;
    Ok(vec![crate::event::BroadcastEnvelope {
        id,
        event_version: crate::event::SYNC_EVENT_VERSION,
        actor: ActorId::Kernel,
        scope: scope.clone(),
        event,
    }])
}

pub(super) async fn append_wake(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wake: Option<(EventScope, Event)>,
) -> Result<Option<crate::event::BroadcastEnvelope>> {
    let Some((scope, mut event)) = wake else {
        return Ok(None);
    };
    if let Event::TrackWakeRequested {
        track_id,
        key,
        text,
        ..
    } = &mut event
        && let Some(task) = crate::db::sqlite::task_get_tx(tx, key).await?
    {
        if task.kind != crate::model::TaskKind::Compact || task.track_id != track_id.as_str() {
            return Err(CalmError::Conflict("compaction task owner mismatch".into()));
        }
        // Read the claim-frozen row in the same transaction as settlement and wake.
        // Template substitutions have already finished in prepare_wake; never render
        // the handoff as a template or trim its original whitespace.
        match serde_json::from_str::<serde_json::Value>(&task.context_json)? {
            serde_json::Value::String(handoff) => {
                text.push_str(include_str!(
                    "../../../prompts/planner_compaction_handoff.md"
                ));
                text.push_str(&handoff);
            }
            // Explicit historical branch: compact rows claimed before handoffs.
            serde_json::Value::Object(context) if context.is_empty() => {}
            _ => {
                return Err(CalmError::Conflict(
                    "invalid persisted compaction context".into(),
                ));
            }
        }
    }
    let id =
        crate::db::sqlite::append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event)
            .await?;
    Ok(Some(crate::event::BroadcastEnvelope {
        id,
        event_version: crate::event::SYNC_EVENT_VERSION,
        actor: ActorId::Kernel,
        scope,
        event,
    }))
}

pub(super) async fn publish_wake(
    inner: &Inner,
    committed: Option<PlannerCompaction>,
    wake: Option<crate::event::BroadcastEnvelope>,
) {
    // A public checkpoint can overlap an actor-owned request or settlement.
    // Publish only the marker this transaction committed, never its stale record.
    if wake.is_some()
        && let Some(committed) = committed
        && let Some(current) = inner.planner_compaction.lock().await.as_mut()
        && current.request_id == committed.request_id
        && current.phase == committed.phase
    {
        current.wake_enqueued = true;
    }
    if let Some(wake) = wake {
        inner.events.emit_envelope(wake);
    }
}
