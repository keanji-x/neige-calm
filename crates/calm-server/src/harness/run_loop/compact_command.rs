//! A manual provider compaction, serialized with sends, edits and shutdown.
use super::*;
use provider::control::CompactSubmission;

pub(super) mod canonical;

/// Both maintenance owners use the same existing start and completion budgets.
fn observation_budget(state: &HarnessState, config: &HarnessConfig) -> Option<(Instant, Duration)> {
    match state {
        HarnessState::Compacting { since } => Some((*since, config.interrupt_completion_budget)),
        HarnessState::CompactionRunning { started_at, .. } => {
            Some((*started_at, config.max_turn_duration))
        }
        _ => None,
    }
}

pub(super) fn observation_deadline(
    state: &HarnessState,
    config: &HarnessConfig,
) -> Option<Instant> {
    observation_budget(state, config).and_then(|(since, budget)| since.checked_add(budget))
}

pub(super) fn observation_expired(state: &HarnessState, config: &HarnessConfig) -> bool {
    observation_budget(state, config).is_some_and(|(since, budget)| since.elapsed() >= budget)
}

/// Move only a real, installed maintenance clock. Event-driven timers must be
/// rescheduled when a test changes that clock instead of advancing wall time.
#[cfg(test)]
impl PlannerHarness {
    pub(crate) async fn rewind_compaction_clock_for_test(&self, by: Duration) {
        let mut state = self.inner.state.lock().await;
        match &mut *state {
            HarnessState::Compacting { since: at }
            | HarnessState::CompactionRunning { started_at: at, .. } => {
                *at = at
                    .checked_sub(by)
                    .expect("representable maintenance clock rewind");
            }
            _ => panic!("a real maintenance operation must install its clock first"),
        }
        drop(state);
        self.request_progress();
    }
}

pub(super) enum Source {
    Historical,
    Canonical(crate::auth::HumanInputSource),
}

pub(super) async fn handle_compact(inner: &Arc<Inner>, source: Source) -> Result<()> {
    match source {
        Source::Canonical(source) => return canonical::start(inner, source).await,
        Source::Historical => {
            inner.historical_queue()?;
        }
    }
    let admitted = admit_historical(inner).await?;
    start(inner, admitted, Origin::Human).await
}

pub(super) enum Origin {
    Human,
    PlannerRequest,
}

/// Holds both existing fences from admission through the Start write.
pub(super) struct Admitted<'a> {
    thread: String,
    track_guard: crate::provider_execution::TrackAdmissionFence,
    _issuance: tokio::sync::MutexGuard<'a, ()>,
}

pub(super) async fn admit_historical(inner: &Arc<Inner>) -> Result<Admitted<'_>> {
    inner.historical_queue()?;
    use super::issuance::TrackIssuance;
    let track_guard = match super::issuance::lock_track_for_issuance(inner).await? {
        TrackIssuance::Acquired(guard) => guard,
        TrackIssuance::ShuttingDown => {
            return Err(CalmError::Conflict(
                "the conversation is shutting down".into(),
            ));
        }
        TrackIssuance::Busy => {
            return Err(CalmError::Conflict(
                "Wait for the current turn to finish before compacting.".into(),
            ));
        }
        TrackIssuance::Superseded => {
            return Err(CalmError::PlannerHarnessRuntimeSuperseded(
                "The conversation changed; compaction was not submitted.".into(),
            ));
        }
    };
    let _issuance = inner.issuance.lock().await;
    if inner.parked.is_parked() {
        return Err(CalmError::Conflict("carrier is parked".into()));
    }
    crate::conversation_archive::require_unarchived(inner.repo.as_ref(), inner.card_id.as_str())
        .await?;
    if let Some(reader) = inner.backend.issuance_hold() {
        return Err(CalmError::Conflict(reader));
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Err(CalmError::Conflict(
            "the conversation is shutting down".into(),
        ));
    }
    if !inner.state.lock().await.can_issue_turn() {
        return Err(CalmError::Conflict(
            "Wait for the current turn to finish before compacting.".into(),
        ));
    }
    let thread = inner.thread_id.read().await.clone().ok_or_else(|| {
        CalmError::Conflict("Send a message before compacting this conversation.".into())
    })?;
    if inner.backend.thread_sealed(&thread) {
        return Err(CalmError::Conflict(
            "the conversation is being deleted".into(),
        ));
    }
    if inner.backend.active_turn_id_for_thread(&thread).is_some()
        || !inner.historical_queue()?.lock().await.is_empty()
        || inner.projection_client_id.lock().await.is_some()
        || inner.pending_rewind.lock().await.is_some()
    {
        return Err(CalmError::Conflict(
            "Wait for pending messages and edits to finish before compacting.".into(),
        ));
    }
    if inner.last_turn_id.lock().await.is_none() {
        return Err(CalmError::Conflict(
            "Send a message before compacting this conversation.".into(),
        ));
    }
    Ok(Admitted {
        thread,
        track_guard,
        _issuance,
    })
}

pub(super) async fn start(
    inner: &Arc<Inner>,
    admitted: Admitted<'_>,
    origin: Origin,
) -> Result<()> {
    let Admitted {
        thread,
        track_guard,
        _issuance,
    } = admitted;
    use crate::harness::compact_outcome::{CompactOutcome, CompactStatus};
    let previous_outcome = inner.compact_outcome.lock().await.clone();
    *inner.compact_outcome.lock().await = Some(CompactOutcome {
        operation_id: crate::model::new_id(),
        thread_id: thread.clone(),
        turn_id: None,
        status: CompactStatus::Started,
    });
    let previous_request = inner.planner_compaction.lock().await.clone();
    if matches!(origin, Origin::PlannerRequest) {
        let operation_id = inner
            .compact_outcome
            .lock()
            .await
            .as_ref()
            .expect("Start outcome")
            .operation_id
            .clone();
        let mut request = inner.planner_compaction.lock().await;
        request.as_mut().expect("admitted request").phase =
            crate::harness::compact_outcome::PlannerCompactionPhase::Submitted { operation_id };
    }
    let previous = inner.state.lock().await.clone();
    *inner.state.lock().await = HarnessState::Compacting {
        since: Instant::now(),
    };
    match persist_snapshot_inner(inner, None, None, matches!(origin, Origin::PlannerRequest)).await
    {
        Ok(true) => {}
        result => {
            *inner.state.lock().await = previous;
            *inner.compact_outcome.lock().await = previous_outcome;
            *inner.planner_compaction.lock().await = previous_request;
            return Err(match result {
                Err(error) => error,
                _ => CalmError::PlannerHarnessRuntimeSuperseded(
                    "The conversation changed; compaction was not submitted.".into(),
                ),
            });
        }
    }
    let track_fence = crate::provider_execution::TrackFenceHandoff::new(track_guard);
    let submission = match inner.backend.compact_request(inner.as_ref()).await {
        Ok(request) => {
            IssueTurnHandle::from_reconciliation(inner)
                .await
                .compact_start(request, &track_fence)
                .await
        }
        Err(error) => Err(error),
    };
    let (state, error) = match submission {
        Ok(CompactSubmission::Submitted { .. }) => return Ok(()),
        Err(error) | Ok(CompactSubmission::NotSubmitted { error, .. }) => {
            settle(inner, CompactStatus::Refused).await;
            (previous, error)
        }
        Ok(CompactSubmission::Unconfirmed { error, .. }) => {
            settle(inner, CompactStatus::Unconfirmed).await;
            (HarnessState::Wedged { since: Instant::now(), reason: "compaction submission could not be confirmed; reset the conversation to continue".into() }, error)
        }
    };
    *inner.state.lock().await = state;
    persist_snapshot(inner).await?;
    Err(error)
}

pub(super) async fn settle(
    inner: &Arc<Inner>,
    status: crate::harness::compact_outcome::CompactStatus,
) {
    if let Some(outcome) = inner.compact_outcome.lock().await.as_mut()
        && outcome.status == crate::harness::compact_outcome::CompactStatus::Started
    {
        outcome.status = status.clone();
        if let Some(request) = inner.planner_compaction.lock().await.as_mut()
            && matches!(&request.phase, crate::harness::compact_outcome::PlannerCompactionPhase::Submitted { operation_id } if operation_id == &outcome.operation_id)
        {
            request.phase = crate::harness::compact_outcome::PlannerCompactionPhase::Settled {
                operation_id: outcome.operation_id.clone(),
                status,
            };
        }
    }
}

/// The user watermark at context replacement, never a provider maintenance id.
#[derive(Clone, Debug)]
pub(super) struct UsageInvalidation {
    pub user_turn_id: Option<String>,
}

pub(super) async fn invalidate_usage(inner: &Arc<Inner>, turn_id: Option<String>) {
    *inner.context_usage_invalidation.lock().await = Some(UsageInvalidation {
        user_turn_id: turn_id,
    });
}
