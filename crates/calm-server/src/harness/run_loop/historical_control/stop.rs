//! Frozen historical Stop target and commit-before-publish transition. The
//! existing interrupt state, deadline and native effect have one implementation.
use super::*;
use crate::session_projection_repo::WorkerSessionProjection;
use calm_types::harness::{HarnessControlIntent, HarnessInterruptionIntent, HarnessStopRequest};

#[derive(Clone)]
enum Requester {
    Human(HistoricalHumanSource),
    /// Only a validated old prepared operation can construct this request.
    CommittedLegacy,
}

#[derive(Clone)]
pub(in crate::harness::run_loop) struct PreparedStop {
    request: HarnessStopRequest,
    requester: Requester,
    reason: String,
    state: HarnessState,
}

/// Interpret the complete committed projection at both selection and commit.
/// Its phase and accepted cause cannot be inferred from target IDs alone.
struct RuntimeTarget<'a> {
    phase: HarnessPhaseTag,
    thread: &'a str,
    turn: &'a str,
    intent: Option<HarnessInterruptionIntent>,
}

impl<'a> RuntimeTarget<'a> {
    fn read(runtime: &'a WorkerSessionProjection) -> Result<Option<Self>> {
        let value = runtime.handle_state_json.clone().ok_or_else(|| {
            CalmError::Conflict("original Stop has no recorded harness snapshot".into())
        })?;
        let snapshot = match HarnessRuntimeSnapshot::decode(value)?.kind {
            RuntimeSnapshotKind::Historical(snapshot) => snapshot,
            RuntimeSnapshotKind::Canonical => {
                return Err(CalmError::Conflict(
                    "original historical Stop cannot use a canonical snapshot".into(),
                ));
            }
        };
        let phase = snapshot.phase;
        if matches!(
            phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ) && runtime.active_turn_id.is_none()
        {
            return Ok(None);
        }
        let (Some(thread), Some(turn)) = (&runtime.thread_id, &runtime.active_turn_id) else {
            return Err(CalmError::Conflict(
                "original Stop has no confirmed native target".into(),
            ));
        };
        if thread.is_empty()
            || turn.is_empty()
            || !matches!(
                phase,
                HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingInterrupt
            )
            || snapshot.last_thread_id.as_ref() != Some(thread)
            || snapshot.last_turn_id.as_ref() != Some(turn)
            || snapshot
                .interruption_intent
                .as_ref()
                .is_some_and(|intent| intent.turn_id() != turn)
        {
            return Err(CalmError::Conflict(
                "original Stop target contradicts its recorded snapshot".into(),
            ));
        }
        Ok(Some(Self {
            phase,
            thread,
            turn,
            intent: snapshot.interruption_intent,
        }))
    }
}

impl PreparedStop {
    fn from_runtime(
        inner: &Inner,
        runtime: &WorkerSessionProjection,
        requester: Requester,
        reason: String,
        requested_at_ms: i64,
    ) -> Result<Option<Self>> {
        if runtime.id != inner.worker_session_id || runtime.card_id != inner.card_id.as_str() {
            return Err(CalmError::Conflict(
                "original Stop names another runtime".into(),
            ));
        }
        let Some(target) = RuntimeTarget::read(runtime)? else {
            return Ok(None);
        };
        let plan = Self {
            request: HarnessStopRequest {
                session_id: runtime.id.clone(),
                card_id: runtime.card_id.clone(),
                track_id: inner.track_id.to_string(),
                thread_id: target.thread.to_owned(),
                turn_id: target.turn.to_owned(),
                requested_at_ms,
            },
            requester,
            state: interrupt_state(target.turn, &reason),
            reason,
        };
        Ok(plan.needs_commit(runtime, &target)?.then_some(plan))
    }

    pub(in crate::harness::run_loop) fn state(&self) -> &HarnessState {
        &self.state
    }
    pub(in crate::harness::run_loop) fn intent(&self) -> HarnessInterruptionIntent {
        HarnessInterruptionIntent::Control(HarnessControlIntent::UserStopV1(self.request.clone()))
    }

    fn needs_commit(
        &self,
        runtime: &WorkerSessionProjection,
        target: &RuntimeTarget<'_>,
    ) -> Result<bool> {
        if runtime.id != self.request.session_id
            || runtime.card_id != self.request.card_id
            || target.thread != self.request.thread_id
            || target.turn != self.request.turn_id
        {
            return Err(CalmError::Conflict(
                "original Stop target is no longer current".into(),
            ));
        }
        match &self.requester {
            // A new human request cannot replace an accepted cause, extend its
            // deadline or treat ACK without END as another native interrupt.
            Requester::Human(_) => {
                Ok(target.phase != HarnessPhaseTag::IssuingInterrupt && target.intent.is_none())
            }
            // An old receipt may finish its own exact promise after a crash.
            // Neither an unrelated accepted cause nor an unproved interrupt
            // phase grants that receipt a new Stop intent.
            Requester::CommittedLegacy => match &target.intent {
                Some(intent) if intent == &self.intent() => Ok(true),
                None if target.phase == HarnessPhaseTag::TurnRunning => Ok(true),
                _ => Err(CalmError::Conflict(
                    "old interruption does not own the accepted Stop intent".into(),
                )),
            },
        }
    }

    pub(in crate::harness::run_loop) async fn needs_commit_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<bool> {
        // Even a no-op is a new request and must pass current authorization.
        match &self.requester {
            Requester::Human(source) => source.require_card_tx(tx, &self.request.card_id).await?,
            Requester::CommittedLegacy => {
                crate::conversation_inputs::require_historical_storage_tx(
                    tx,
                    &self.request.card_id,
                )
                .await?;
            }
        }
        let current =
            crate::db::sqlite::session_projection_active_for_card_tx(tx, &self.request.card_id)
                .await?
                .ok_or_else(|| {
                    CalmError::Conflict("original Stop runtime is no longer current".into())
                })?;
        let target = RuntimeTarget::read(&current)?.ok_or_else(|| {
            CalmError::Conflict("original Stop target is no longer current".into())
        })?;
        self.needs_commit(&current, &target)
    }
}

pub(in crate::harness::run_loop) fn interrupt_state(turn: &str, reason: &str) -> HarnessState {
    HarnessState::Issuing {
        since: Instant::now(),
        kind: IssuingKind::Interrupt {
            target_turn_id: turn.to_owned(),
            reason: reason.to_owned(),
        },
    }
}

pub(in crate::harness::run_loop) async fn adopt_interrupt(
    inner: &Arc<Inner>,
    state: HarnessState,
    intent: Option<HarnessInterruptionIntent>,
    turn: &str,
) {
    *inner.issued_turn_id.lock().await = None;
    *inner.interruption_intent.lock().await = intent;
    *inner.state.lock().await = state;
    let mut deadline = inner.interrupt_deadline.lock().await;
    if !deadline
        .as_ref()
        .is_some_and(|(existing, _)| existing == turn)
    {
        *deadline = Some((
            turn.to_owned(),
            Instant::now() + inner.config.interrupt_completion_budget,
        ));
    }
}

pub(in crate::harness::run_loop) async fn dispatch_interrupt(
    inner: &Arc<Inner>,
    thread: &str,
    turn: &str,
    reason: &str,
) {
    if let Err(error) = inner.backend.turn_interrupt(thread, turn).await {
        tracing::warn!(thread_id=thread,turn_id=turn,reason,%error,
            "planner harness turn/interrupt failed; interrupt timeout watchdog remains armed");
    }
}

impl PlannerHarness {
    pub(crate) async fn stop_historical(&self, source: HistoricalHumanSource) -> Result<bool> {
        self.inner.historical_queue()?;
        let (inner, source) = (self.inner.clone(), source.clone());
        let repo = inner.repo.clone();
        let plan = write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                source.require_card_tx(tx, inner.card_id.as_str()).await?;
                let runtime = crate::db::sqlite::session_projection_active_for_card_tx(
                    tx,
                    inner.card_id.as_str(),
                )
                .await?
                .ok_or_else(|| {
                    CalmError::Conflict("historical Stop runtime is no longer current".into())
                })?;
                PreparedStop::from_runtime(
                    &inner,
                    &runtime,
                    Requester::Human(source),
                    "user_stop".into(),
                    crate::model::now_ms(),
                )
            })
        })
        .await?;
        let Some(plan) = plan else { return Ok(false) };
        self.stop_original(plan).await
    }

    /// Compatibility for a proved old prepared receipt. No current turn is ever
    /// substituted for a missing or completed original target.
    pub(crate) async fn interrupt_original_receipt(
        &self,
        runtime: &WorkerSessionProjection,
        reason: String,
        requested_at_ms: i64,
    ) -> Result<()> {
        self.inner.historical_queue()?;
        let plan = PreparedStop::from_runtime(
            &self.inner,
            runtime,
            Requester::CommittedLegacy,
            reason,
            requested_at_ms,
        )?
        .ok_or_else(|| {
            CalmError::Conflict("old interruption has no original running target".into())
        })?;
        if self.stop_original(plan).await? {
            Ok(())
        } else {
            Err(CalmError::Conflict(
                "old interruption was not accepted".into(),
            ))
        }
    }

    async fn stop_original(&self, plan: PreparedStop) -> Result<bool> {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            crate::test_seams::HISTORICAL_STOP_SELECTED,
            self.inner.card_id.as_str(),
        )
        .await;
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, result) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::HistoricalInterrupt {
                        plan: Box::new(plan),
                        answer,
                    })
                    .map_err(map_observation_send_error)?;
                result
                    .await
                    .map_err(|_| CalmError::Conflict("original Stop owner closed".into()))?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => Err(CalmError::Conflict(
                "original Stop owner is not running".into(),
            )),
        }
    }
}

pub(in crate::harness::run_loop) async fn handle(
    inner: &Arc<Inner>,
    plan: PreparedStop,
    answer: oneshot::Sender<Result<bool>>,
) {
    let result = persist_snapshot_inner(inner, SnapshotWrite::Stop(plan.clone())).await;
    match result {
        Ok(SnapshotWriteOutcome::Written) => {
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point(
                crate::test_seams::HISTORICAL_STOP_COMMITTED,
                inner.card_id.as_str(),
            )
            .await;
            // No in-memory acceptance fact changes until the original writer
            // commits. A later logout cannot revoke the accepted exact drain.
            adopt_interrupt(
                inner,
                plan.state.clone(),
                Some(plan.intent()),
                &plan.request.turn_id,
            )
            .await;
            publish_historical_phase(inner, HarnessPhaseTag::IssuingInterrupt).await;
            let inner = inner.clone();
            let request = plan.request;
            let reason = plan.reason;
            tokio::spawn(async move {
                dispatch_interrupt(&inner, &request.thread_id, &request.turn_id, &reason).await;
                let _ = answer.send(Ok(true));
            });
        }
        Ok(SnapshotWriteOutcome::StopAlreadyAccepted) => {
            let _ = answer.send(Ok(false));
        }
        Ok(SnapshotWriteOutcome::Superseded) => {
            let _ = answer.send(Err(CalmError::Conflict(
                "original Stop runtime was superseded".into(),
            )));
        }
        Err(error) => {
            let _ = answer.send(Err(error));
        }
    }
}
