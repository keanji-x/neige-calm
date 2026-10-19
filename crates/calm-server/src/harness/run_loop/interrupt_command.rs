//! Stop commands belong to the run-loop/issuance owner. The separate lane is also polled by
//! that owner during turn/start: a provider may need an interrupt before start returns.
use super::*;

pub(super) struct Command {
    target: Option<Target>,
    reason: String,
    answer: oneshot::Sender<Result<()>>,
}

pub(super) struct Ingress {
    sender: mpsc::Sender<Command>,
    pub(super) checkpoint_failed: AtomicBool,
    pub(super) receiver: StdMutex<Option<mpsc::Receiver<Command>>>,
}

impl Default for Ingress {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(OBSERVATION_BUFFER);
        Self {
            sender,
            checkpoint_failed: AtomicBool::new(false),
            receiver: StdMutex::new(Some(receiver)),
        }
    }
}

#[derive(Clone)]
pub(super) struct Target {
    turn_id: String,
    // A start-in-flight has no harness turn id yet. Bind the provider-reported id to that
    // exact issuance, rather than looking up whatever turn happens to be active later.
    start_since: Option<Instant>,
}

impl Target {
    pub(super) fn running(turn_id: String) -> Self {
        Self {
            turn_id,
            start_since: None,
        }
    }

    async fn sample(inner: &Inner) -> Option<Self> {
        let state = inner.state.lock().await.clone();
        match &state {
            HarnessState::TurnRunning { turn_id, .. } => Some(Self::running(turn_id.clone())),
            HarnessState::Issuing {
                since,
                kind: IssuingKind::TurnStart,
            } => {
                let thread = inner.thread_id.read().await.clone()?;
                Some(Self {
                    turn_id: inner.backend.active_turn_id_for_thread(&thread)?,
                    start_since: Some(*since),
                })
            }
            _ => None,
        }
    }

    fn matches(&self, state: &HarnessState, active: Option<&str>) -> bool {
        match state {
            HarnessState::TurnRunning { turn_id, .. } => *turn_id == self.turn_id,
            HarnessState::Issuing {
                since,
                kind: IssuingKind::TurnStart,
            } => self.start_since == Some(*since) && active == Some(self.turn_id.as_str()),
            _ => false,
        }
    }
}

pub(super) async fn submit(harness: &PlannerHarness, reason: String) -> Result<()> {
    if harness.inner.shutting_down.load(Ordering::SeqCst) {
        return Err(CalmError::Conflict(
            "interrupt owner is shutting down".into(),
        ));
    }
    handoff_pause::require_unpaused(&harness.inner).await?;
    let target = Target::sample(&harness.inner).await;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("interrupt-target-sampled", harness.inner.card_id.as_str())
        .await;
    let (answer, reply) = oneshot::channel();
    let command = Command {
        target,
        reason,
        answer,
    };
    match &harness.inner.observations {
        ObservationIngress::Running(_) => {
            harness
                .inner
                .interrupt_commands
                .sender
                .try_send(command)
                .map_err(|_| {
                    CalmError::Conflict(
                        "interrupt owner unavailable; refresh the conversation".into(),
                    )
                })?;
            reply.await.map_err(|_| {
                CalmError::Conflict("interrupt reply lost; refresh the conversation".into())
            })?
        }
        #[cfg(feature = "fixtures")]
        ObservationIngress::Unstarted(_) => {
            let _issuance = harness.inner.issuance.lock().await;
            apply(&harness.inner, command.target, command.reason).await
        }
    }
}

/// Normal loop entry; never call this while already holding issuance.
pub(super) async fn handle(inner: &Arc<Inner>, command: Command) {
    let _issuance = inner.issuance.lock().await;
    handle_owned(inner, command).await;
}

/// The turn/start future's owner already holds issuance; do not enqueue to ourselves or relock.
pub(super) async fn handle_owned(inner: &Arc<Inner>, command: Command) {
    let result = apply(inner, command.target, command.reason).await;
    let _ = command.answer.send(result);
}

pub(super) async fn watchdog(inner: &Arc<Inner>, turn: String) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
    apply(
        inner,
        Some(Target::running(turn)),
        "max_turn_duration".into(),
    )
    .await
}

async fn apply(inner: &Arc<Inner>, target: Option<Target>, reason: String) -> Result<()> {
    // These are final admission checks in the same owner as completion and pause, including for
    // requests sampled before pause. A stale target is never resolved against a new turn.
    handoff_pause::require_unpaused(inner).await?;
    let current = inner
        .repo
        .session_projection_by_id(&inner.worker_session_id)
        .await?;
    if inner.shutting_down.load(Ordering::SeqCst)
        || !current.is_some_and(|row| {
            row.card_id == inner.card_id.as_str() && row.status.is_active_authority()
        })
    {
        return Err(CalmError::Conflict(
            "interrupt owner is no longer current".into(),
        ));
    }
    let Some(target) = target else { return Ok(()) };
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        return Ok(());
    };
    let before = inner.state.lock().await.clone();
    if !target.matches(
        &before,
        inner
            .backend
            .active_turn_id_for_thread(&thread_id)
            .as_deref(),
    ) {
        return Ok(());
    }
    let issued = inner.issued_turn_id.lock().await.clone();
    let intent = inner.interruption_intent.lock().await.clone();
    let deadline = inner.interrupt_deadline.lock().await.clone();
    let target_turn_id = target.turn_id;
    *inner.issued_turn_id.lock().await = None;
    if reason == "max_turn_duration" {
        use calm_types::harness::{
            HarnessInterruptionIntent, HarnessInterruptionReason, HarnessTimeoutIntent,
            HarnessTimeoutRequest,
        };
        if let HarnessState::TurnRunning { watchdog_from, .. } = &before {
            *inner.interruption_intent.lock().await = Some(HarnessInterruptionIntent::Versioned(
                HarnessTimeoutIntent::V1(HarnessTimeoutRequest {
                    session_id: inner.worker_session_id.clone(),
                    card_id: inner.card_id.to_string(),
                    track_id: inner.track_id.to_string(),
                    thread_id: thread_id.clone(),
                    turn_id: target_turn_id.clone(),
                    reason: HarnessInterruptionReason::MaxTurnDuration,
                    requested_at_ms: crate::model::now_ms(),
                    effective_elapsed_ms: watchdog_from
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .expect("elapsed fits u64"),
                    budget_ms: budget::limit(inner)
                        .await
                        .as_millis()
                        .try_into()
                        .expect("budget fits u64"),
                }),
            ));
        }
    } else {
        *inner.interruption_intent.lock().await = None;
    }
    *inner.state.lock().await = HarnessState::Issuing {
        since: Instant::now(),
        kind: IssuingKind::Interrupt {
            target_turn_id: target_turn_id.clone(),
            reason: reason.clone(),
        },
    };
    *inner.interrupt_deadline.lock().await = Some((
        target_turn_id.clone(),
        Instant::now() + inner.config.interrupt_completion_budget,
    ));
    match persist_snapshot_inner(inner, None, None).await {
        Ok(true) => {}
        result => {
            *inner.state.lock().await = before;
            *inner.issued_turn_id.lock().await = issued;
            *inner.interruption_intent.lock().await = intent;
            *inner.interrupt_deadline.lock().await = deadline;
            // A failed commit may be unknown. Restore staged memory, stop writes and let recovery
            // reload SQL; never send a provider stop based on an unconfirmed checkpoint.
            inner
                .interrupt_commands
                .checkpoint_failed
                .store(true, Ordering::SeqCst);
            close_observation_ingress(inner);
            return result.map(|_| ()).and(Err(CalmError::Conflict(
                "interrupt checkpoint was not confirmed".into(),
            )));
        }
    }
    let outcome = IssueTurnHandle::from_reconciliation(inner)
        .await
        .turn_interrupt(InterruptRequest {
            turn: inner.backend.turn_identity(target_turn_id.clone()),
        })
        .await;
    if let Err(error) = inner.backend.legacy_interrupt_result(outcome) {
        tracing::warn!(thread_id, target_turn_id, reason, %error, "turn interrupt failed; completion watchdog remains armed");
    }
    Ok(())
}
