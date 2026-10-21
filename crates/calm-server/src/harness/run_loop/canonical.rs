//! Canonical inputs are read and claimed from the current carrier. This runtime
//! stores only transient exact-input sources and a committed read projection.
use super::*;
use crate::conversation_inputs::InputSource;
use crate::db::sqlite::conversation::{
    CurrentInputCarrier, RuntimeFrameAfterWrite, pending_plan_for_prefix_tx,
    persist_runtime_frame_tx,
};
use crate::db::sqlite::conversation::{
    RuntimeExecutionFence, current_input_carrier_tx, current_input_fact_carrier_tx,
};
use crate::harness::profile::input_carrier_binding_tx;
use crate::harness::registry::CanonicalSourceIndex;
use crate::harness::snapshot::{
    CanonicalHarnessSnapshot, canonical_runtime_header_value as header_value,
};
use crate::planner_permission_mode::PlannerPermissionMode;
use calm_types::conversation::{
    CanonicalPendingIdentity, InputDeliveryFact, PendingInput, SystemRenderContext,
};
use provider::TurnModelSelection;

mod content;
mod load;
pub(super) use load::load_current;
mod resume;
mod retry;
use resume::resume_unattempted;
pub(super) mod control;
pub(super) use content::{invalidate_context_usage, on_notification, persist_native_item};

#[cfg(all(test, feature = "fixtures"))]
mod retry_tests;
#[cfg(all(test, feature = "fixtures"))]
mod tests;

fn frame(carrier: &CurrentInputCarrier) -> Result<CommittedFrame> {
    Ok(CommittedFrame {
        snapshot: checked_snapshot(carrier.snapshot().clone())?,
        fence: RuntimeExecutionFence::from_carrier(carrier),
    })
}

pub(super) fn written(frame: RuntimeFrameAfterWrite) -> Result<CommittedFrame> {
    Ok(CommittedFrame {
        snapshot: checked_snapshot(frame.snapshot)?,
        fence: frame.execution_fence,
    })
}

fn checked_snapshot(value: Value) -> Result<CanonicalHarnessSnapshot> {
    let snapshot: CanonicalHarnessSnapshot = serde_json::from_value(value)?;
    if snapshot.header.phase == HarnessPhaseTag::IssuingInterrupt {
        let turn = snapshot
            .header
            .last_turn_id
            .as_deref()
            .filter(|turn| !turn.is_empty());
        if turn.is_none()
            || snapshot
                .header
                .last_thread_id
                .as_deref()
                .is_none_or(str::is_empty)
            || snapshot
                .header
                .interruption_intent
                .as_ref()
                .is_none_or(|intent| Some(intent.turn_id()) != turn)
        {
            return Err(CalmError::Conflict(
                "canonical control checkpoint has no exact original target/intent".into(),
            ));
        }
    }
    Ok(snapshot)
}

#[derive(Clone)]
pub(super) struct CommittedFrame {
    pub snapshot: CanonicalHarnessSnapshot,
    pub fence: RuntimeExecutionFence,
}

/// Equality watermark for the existing phase-refresh emitter. No bodies, grants,
/// private native receipts, or another runtime state machine are retained here.
#[derive(Clone, PartialEq)]
pub(super) struct PublicFeedback {
    pending: Vec<(CanonicalPendingIdentity, u64, i64)>,
    active: Option<calm_types::conversation::ActiveInputSubmissions>,
    attempts: Vec<calm_types::conversation::InputAttemptCheckpoint>,
    receipts: Vec<calm_types::conversation::HumanInputResolutionReceipt>,
    usage: Option<(i64, Option<i64>, Option<f64>, i64)>,
    blocked: Option<String>,
    native_turn: Option<String>,
}
impl PublicFeedback {
    fn from_frame(frame: &CanonicalHarnessSnapshot, standing_reason: Option<String>) -> Self {
        let h = &frame.header;
        Self {
            pending: frame
                .canonical_inputs
                .pending
                .iter()
                .map(|p| (p.identity(), p.queue_position(), p.queued_at_ms()))
                .collect(),
            active: frame.canonical_inputs.active.clone(),
            attempts: frame.canonical_inputs.attempts.clone(),
            receipts: frame.canonical_inputs.receipts.clone(),
            usage: if h.context_usage_invalidated {
                None
            } else {
                h.token_usage
                    .as_ref()
                    .map(|u| (u.used_tokens, u.context_window, u.percent(), u.at_ms))
            },
            blocked: h.wedged_reason.clone().or(standing_reason),
            native_turn: (h.phase == HarnessPhaseTag::TurnRunning)
                .then(|| h.last_turn_id.clone())
                .flatten(),
        }
    }
}

pub(super) struct CanonicalInputs {
    // This is an identity index, not an ordered queue or an admission cache.
    // Every claim/crossing still revalidates its actual issuer and current scope.
    sources: Arc<CanonicalSourceIndex>,
    projection: Mutex<CommittedFrame>,
}

impl CanonicalInputs {
    pub fn new(frame: CommittedFrame, sources: Arc<CanonicalSourceIndex>) -> Self {
        Self {
            sources,
            projection: Mutex::new(frame),
        }
    }

    pub async fn observe_source(&self, input: CanonicalPendingIdentity, source: InputSource) {
        self.sources.observe(input, source);
    }

    pub async fn source(&self, input: &CanonicalPendingIdentity) -> Option<InputSource> {
        self.sources.source(input)
    }

    pub async fn committed(&self) -> CommittedFrame {
        self.projection.lock().await.clone()
    }

    pub async fn publish_committed(&self, frame: CommittedFrame) {
        *self.projection.lock().await = frame;
    }
}

pub(super) async fn read_current(inner: &Inner) -> Result<CommittedFrame> {
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move { frame(&current_input_fact_carrier_tx(tx, &card, &runtime).await?) })
    })
    .await
}

/// This cache is published only from a returned, committed SQL result. Native
/// start/end notifications are triggers, never an alternate phase writer.
pub(super) async fn publish(inner: &Inner, next: CommittedFrame) {
    let inputs = inner.inputs.canonical().expect("canonical consumer");
    let header = &next.snapshot.header;
    let mut state = inner.state.lock().await;
    let same_running = matches!(&*state, HarnessState::TurnRunning {turn_id,..}
        if header.phase == HarnessPhaseTag::TurnRunning && header.last_turn_id.as_ref() == Some(turn_id));
    let same_interrupt = matches!(&*state, HarnessState::Issuing {kind:IssuingKind::Interrupt {target_turn_id,..},..}
        if header.phase == HarnessPhaseTag::IssuingInterrupt && header.last_turn_id.as_ref() == Some(target_turn_id));
    if !same_running && !same_interrupt {
        *state = if header.phase == HarnessPhaseTag::TurnRunning {
            match &header.last_turn_id {
                Some(turn) => HarnessState::turn_running(turn.clone(), Instant::now()),
                None => HarnessState::Resumed {
                    resumed_at: Instant::now(),
                },
            }
        } else if header.phase == HarnessPhaseTag::IssuingInterrupt {
            HarnessState::Issuing {
                since: Instant::now(),
                kind: IssuingKind::Interrupt {
                    target_turn_id: header
                        .last_turn_id
                        .clone()
                        .expect("committed control has its target"),
                    reason: if header
                        .interruption_intent
                        .as_ref()
                        .and_then(|intent| intent.timeout_reason())
                        .is_some()
                    {
                        "max_turn_duration".into()
                    } else {
                        "user_stop".into()
                    },
                },
            }
        } else {
            state_from_runtime_header(header, &inner.inputs)
        };
    }
    drop(state);
    if header.phase == HarnessPhaseTag::IssuingInterrupt && !same_interrupt {
        *inner.interrupt_deadline.lock().await = header
            .interruption_intent
            .as_ref()
            .and_then(|intent| intent.requested_at_ms())
            .map(|requested| {
                let elapsed = Duration::from_millis(
                    crate::model::now_ms().saturating_sub(requested).max(0) as u64,
                );
                (
                    header.last_turn_id.clone().expect("control target"),
                    Instant::now()
                        + inner
                            .config
                            .interrupt_completion_budget
                            .saturating_sub(elapsed),
                )
            });
    } else if header.phase != HarnessPhaseTag::IssuingInterrupt {
        *inner.interrupt_deadline.lock().await = None;
    }
    *inner.thread_id.write().await = header.last_thread_id.clone();
    *inner.last_turn_id.lock().await = header.last_turn_id.clone();
    *inner.compact_outcome.lock().await = header.compact_outcome.clone();
    *inner.turn_budget.lock().await = header.turn_budget.clone();
    *inner.interruption_intent.lock().await = header.interruption_intent.clone();
    *inner.token_usage.lock().await = header.token_usage.clone();
    *inner.context_usage_invalidation.lock().await =
        header
            .context_usage_invalidated
            .then_some(compact_command::UsageInvalidation {
                user_turn_id: header.context_usage_invalidated_turn.clone(),
            });
    let emitted = (
        header.compact_outcome.clone(),
        header.context_usage_invalidated,
        Some(PublicFeedback::from_frame(
            &next.snapshot,
            inner.issuance_block.lock().await.clone(),
        )),
    );
    let phase = header.phase;
    inputs.publish_committed(next).await;
    emit_phase_refresh(inner, phase, emitted).await;
}

async fn publish_end(inner: &Arc<Inner>, done: RuntimeFrameAfterWrite) -> Result<()> {
    let next = written(done)?;
    if next.snapshot.header.wedged_reason.is_none() {
        *inner.issuance_block.lock().await = None;
        *inner.refusing_since.lock().await = None;
    }
    publish(inner, next).await;
    held::end_turn(inner).await;
    Ok(())
}

fn error_notice(error: &CalmError) -> &'static str {
    match error {
        CalmError::Unauthorized => "A current session must authorize this saved input.",
        CalmError::Forbidden(_) => "You no longer have access to run this saved input.",
        CalmError::Conflict(_) => {
            "This saved input or its conversation changed. Execution is held."
        }
        _ => "Execution is waiting for its saved state to become available.",
    }
}

/// Feedback and pacing are not permission to retry or release an input claim.
pub(super) async fn publish_issuance_error(inner: &Inner, error: &CalmError) {
    *inner.issuance_block.lock().await = Some(error_notice(error).into());
    *inner.issuance_retry_after.lock().await = Some(Instant::now() + TRANSIENT_RETRY_DELAY);
}

/// Consume the one persisted pending order in the existing scheduler. Live
/// source entries carry no body, queue order, or reusable execution grant.
pub(super) async fn tick(inner: &Arc<Inner>) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
    if inner.parked.is_parked() {
        return Ok(());
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    if let Some(done) = crate::harness::input_dispatch::complete_once(
        inner.repo.as_ref(),
        &inner.backend,
        inner.card_id.as_str(),
        &inner.worker_session_id,
    )
    .await?
    {
        publish_end(inner, done).await?;
    }
    if super::compact_command::canonical::observe(inner).await? {
        return Ok(());
    }
    let current = read_current(inner).await?;
    publish(inner, current.clone()).await;
    watchdog_tick(inner).await?;
    let current = read_current(inner).await?;
    let checkpoint = &current.snapshot.canonical_inputs;
    let header = &current.snapshot.header;
    if inner.issuance_paused.load(Ordering::SeqCst)
        || provider_holds_issuance(inner).await
        || inner
            .issuance_retry_after
            .lock()
            .await
            .is_some_and(|after| Instant::now() < after)
        || !inner.held_requests.is_empty()
        || header.last_thread_id.is_none()
        || header.pending_rewind.is_some()
        || header.interruption_intent.is_some()
        || header.wedged_reason.is_some()
    {
        return Ok(());
    }
    if checkpoint.active.is_some() && !checkpoint.resolved_execution_without_target() {
        return resume_unattempted(inner, &current).await;
    }
    if checkpoint.pending.is_empty()
        || !matches!(
            header.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ) && !checkpoint.resolved_execution_without_target()
    {
        return Ok(());
    }
    if let Some(pending) = checkpoint.pending.first()
        && inner
            .inputs
            .canonical()
            .expect("canonical consumer")
            .source(&pending.identity())
            .await
            .is_none()
    {
        publish_issuance_error(inner, &CalmError::Unauthorized).await;
        return Ok(());
    }
    // Like historical issuance, take the track fence before the card issuance
    // lock. Cleaning may run while idle, but not across the durable claim.
    drop(_issuance);
    let track_guard = match super::issuance::lock_track_for_issuance(inner).await? {
        super::issuance::TrackIssuance::Acquired(guard) => guard,
        super::issuance::TrackIssuance::ShuttingDown
        | super::issuance::TrackIssuance::Busy
        | super::issuance::TrackIssuance::Superseded => return Ok(()),
    };
    let _issuance = inner.issuance.lock().await;
    if inner.parked.is_parked() {
        return Ok(());
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    let current = read_current(inner).await?;
    let checkpoint = &current.snapshot.canonical_inputs;
    let header = &current.snapshot.header;
    if inner.issuance_paused.load(Ordering::SeqCst)
        || provider_holds_issuance(inner).await
        || inner
            .issuance_retry_after
            .lock()
            .await
            .is_some_and(|after| Instant::now() < after)
        || !inner.held_requests.is_empty()
        || header.last_thread_id.is_none()
        || header.pending_rewind.is_some()
        || header.interruption_intent.is_some()
        || header.wedged_reason.is_some()
    {
        return Ok(());
    }
    if (checkpoint.active.is_some() && !checkpoint.resolved_execution_without_target())
        || (!matches!(
            header.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ) && !checkpoint.resolved_execution_without_target())
    {
        return Ok(());
    }
    let Some(pending) = checkpoint.pending.first() else {
        return Ok(());
    };
    let inputs = inner.inputs.canonical().expect("canonical consumer");
    let message = pending.identity();
    let Some(source) = inputs.source(&message).await else {
        publish_issuance_error(inner, &CalmError::Unauthorized).await;
        return Ok(());
    };
    if let CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id } = &message {
        return retry::dispatch_pending(inner, &current, source, retry_intent_id.clone()).await;
    }
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let expected = current.fence.clone();
    let message = message.clone();
    // Before model/workspace/config reads, validate the same source and exact
    // head through the shared admission owner. Claim and crossing repeat it.
    let read_admission = write_in_tx_typed(inner.repo.as_ref(), {
        let repo = inner.repo.clone();
        let (card, runtime, source, message, expected) = (
            card.clone(),
            runtime.clone(),
            source.clone(),
            message.clone(),
            expected.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                require_head(&carrier, &expected, &message)?;
                let plan = pending_plan_for_prefix_tx(
                    tx,
                    &carrier,
                    std::num::NonZeroU32::new(1).expect("one input"),
                    vec![],
                )
                .await?;
                crate::conversation_inputs::SelectionReadAdmission::pending_tx(
                    tx, repo, source, &carrier, plan,
                )
                .await
            })
        }
    })
    .await?;
    let resolved = async {
        let budget = budget::sample(inner).await?.map(|sampled| sampled.budget);
        let selection = inner.backend.resolve_selection(&read_admission).await?;
        let permission = turn_permission::resolve(inner).await?;
        Ok::<_, IssuanceRefusal>((budget, selection, permission))
    }
    .await;
    let (budget, selection, permission) = match resolved {
        Ok(resolved) => resolved,
        Err(refusal) => {
            apply_refusal(inner, &refusal).await;
            if let Err(error) = read_admission.revalidate().await {
                publish_issuance_error(inner, &error).await;
            }
            return Ok(());
        }
    };
    if checkpoint
        .requires_settled_activity(header.last_thread_id.as_deref().expect("thread checked"))
        .map_err(|error| CalmError::Conflict(error.into()))?
    {
        let admission = crate::harness::native_admission::NativePromptAdmission::new(
            || read_admission.check_current(),
            || Box::pin(read_admission.revalidate()),
        );
        match inner
            .backend
            .settled_activity(
                inner.card_id.as_str(),
                header.last_thread_id.as_deref().expect("thread checked"),
                &admission,
            )
            .await
        {
            Ok(crate::harness::backend::SettledActivity::Settled) => {}
            Ok(crate::harness::backend::SettledActivity::Held) | Err(_) => return Ok(()),
        }
    }
    let mut contexts = vec![];
    if let Some(budget) = budget {
        contexts.push(
            SystemRenderContext::new(
                HarnessInputPresentation::SystemPlannerTurnBudget,
                budget.initial_input(),
            )
            .map_err(|error| CalmError::Conflict(error.into()))?,
        );
        contexts.push(
            SystemRenderContext::new(
                HarnessInputPresentation::SystemPlannerContextUsage,
                crate::harness::token_usage::planner_context_input(
                    header.token_usage.as_ref(),
                    header.context_usage_invalidated,
                ),
            )
            .map_err(|error| CalmError::Conflict(error.into()))?,
        );
    }
    let (claimed, claimed_frame) = write_in_tx_typed(inner.repo.as_ref(), {
        let source = source.clone();
        move |tx| {
            Box::pin(async move {
                let mut carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                require_head(&carrier, &expected, &message)?;
                let plan = pending_plan_for_prefix_tx(
                    tx,
                    &carrier,
                    std::num::NonZeroU32::new(1).expect("one input"),
                    contexts,
                )
                .await?;
                let claimed =
                    crate::conversation_inputs::claim_initial_tx(tx, &source, &mut carrier, plan)
                        .await?;
                let mut next: CanonicalHarnessSnapshot =
                    serde_json::from_value(carrier.snapshot().clone())?;
                next.header.turn_budget = match budget {
                    Some(budget) => budget::FrozenBudget::Planner {
                        budget,
                        start_attempt: QueueEntryId::from_wire(
                            claimed.submission_id.as_str().into(),
                        ),
                        warning_attempted: false,
                        context_usage: calm_types::harness::budget::FrozenContextUsage::RenderPlan,
                    },
                    None => budget::FrozenBudget::NotApplicable,
                };
                let persisted = persist_runtime_frame_tx(
                    tx,
                    &card,
                    &runtime,
                    &RuntimeExecutionFence::from_carrier(&carrier),
                    header_value(&next)?,
                )
                .await?;
                crate::db::sqlite::session_set_harness_observation_runtime_tx(
                    tx,
                    &runtime,
                    calm_types::worker::WorkerSessionState::TurnPending,
                    next.header.last_thread_id.as_deref(),
                    None,
                )
                .await?;
                Ok((claimed, written(persisted)?))
            })
        }
    })
    .await?;
    drop(track_guard);
    publish(inner, claimed_frame).await;
    dispatch_initial(
        inner,
        &source,
        &claimed.submission_id,
        &claimed.logical_turn_id,
        &selection,
        permission,
    )
    .await
}

async fn dispatch_initial(
    inner: &Arc<Inner>,
    source: &InputSource,
    submission_id: &calm_types::conversation::InputSubmissionId,
    logical_turn_id: &calm_types::conversation::LogicalTurnId,
    selection: &TurnModelSelection,
    permission: PlannerPermissionMode,
) -> Result<()> {
    let committed = crate::harness::input_dispatch::initial(
        inner.repo.as_ref(),
        source,
        inner.card_id.as_str(),
        &inner.worker_session_id,
        submission_id,
        &inner.backend,
        selection,
        permission,
    )
    .await?;
    inner
        .inputs
        .canonical()
        .expect("canonical consumer")
        .sources
        .consumed(&committed.consumed);
    match committed.report.outcome() {
        Ok(_) => {
            *inner.issuance_block.lock().await = None;
            *inner.refusing_since.lock().await = None;
        }
        Err(failure) => {
            let unknown = matches!(
                committed.report.delivery(),
                crate::harness::backend::InputDelivery::Unknown(_)
            );
            let reader = match failure {
                TurnStartFailure::Refused { reader, .. }
                | TurnStartFailure::AwaitingRecovery { reader, .. } => reader.clone(),
                TurnStartFailure::Transient(error) => error_notice(error).into(),
            };
            *inner.issuance_block.lock().await = Some(if unknown {
                "The original execution outcome is unconfirmed. Its input remains held; it will not be sent again automatically.".into()
            } else {
                reader
            });
        }
    }
    if matches!(
        committed.report.delivery(),
        crate::harness::backend::InputDelivery::NotDispatched(_)
    ) {
        // Only the settled proof released input. Pacing is independent of that
        // proof and must not spin the scheduler at its tick frequency.
        *inner.issuance_retry_after.lock().await = Some(Instant::now() + TRANSIENT_RETRY_DELAY);
    }
    // A local write has no native receipt. Only the actual installed driver,
    // associated with the committed original target, proves it can be tracked.
    let local_target = match committed.report.outcome() {
        Ok(identity @ crate::harness::backend::ExecutionIdentity::Local(_)) => {
            let card = inner.card_id.to_string();
            let runtime = inner.worker_session_id.clone();
            let provider = inner.backend.provider();
            let target = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    crate::harness::input_dispatch::execution_target_tx(
                        tx, &card, &runtime, &provider,
                    )
                    .await
                })
            })
            .await?;
            match target {
                Some(target)
                    if target.scope.thread_and_turn().1 == identity.turn_id()
                        && inner.backend.owns_local_execution(&target).await =>
                {
                    Some(target)
                }
                Some(_) | None => None,
            }
        }
        Ok(crate::harness::backend::ExecutionIdentity::Native(_)) | Err(_) => None,
    };
    // Read the exact committed checkpoint, not Result::Ok/Err or an event's
    // apparent status. Unknown with no known execution remains Issuing.
    let attempt = committed.report.attempt().clone();
    let logical_turn_id = logical_turn_id.clone();
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let provider = inner.backend.provider();
    let next = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &runtime).await?;
            let report = carrier
                .checkpoint()
                .attempt(&attempt.submission_id)
                .filter(|report| report.attempt == attempt)
                .ok_or_else(|| CalmError::Conflict("committed dispatch report changed".into()))?;
            let mut next: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone())?;
            if !carrier.is_execution_current() {
                next.header.phase = HarnessPhaseTag::Wedged;
                next.header.wedged_reason.get_or_insert_with(|| {
                    calm_types::harness::HARNESS_RUNTIME_FAILED_REASON.into()
                });
                if let Some(turn) = report
                    .correlation
                    .native_turn_id
                    .as_ref()
                    .or(report.correlation.local_turn_id.as_ref())
                {
                    next.header.last_turn_id = Some(turn.clone());
                }
                let persisted = persist_runtime_frame_tx(
                    tx,
                    &card,
                    &runtime,
                    &RuntimeExecutionFence::from_carrier(&carrier),
                    header_value(&next)?,
                )
                .await?;
                return written(persisted);
            }
            if next.header.phase != HarnessPhaseTag::IssuingTurn {
                return frame(&carrier);
            }
            let local_running = match &local_target {
                Some(target) => {
                    crate::harness::input_dispatch::execution_target_tx(
                        tx, &card, &runtime, &provider,
                    )
                    .await?
                    .as_ref()
                        == Some(target)
                }
                None => false,
            };
            match &report.delivery {
                InputDeliveryFact::NotDispatched { .. }
                    if next.canonical_inputs.active.is_none() =>
                {
                    next.header.phase = HarnessPhaseTag::Idle;
                }
                InputDeliveryFact::Unknown
                    if local_running
                        && local_target.as_ref().is_some_and(|target| {
                            target.logical_turn_id == logical_turn_id
                                && target.proof.attempt() == &attempt
                        })
                        && next.header.last_thread_id.as_deref()
                            == Some(report.correlation.thread_id.as_str()) =>
                {
                    next.header.phase = HarnessPhaseTag::TurnRunning;
                    next.header.last_turn_id = report.correlation.local_turn_id.clone();
                }
                InputDeliveryFact::Accepted { .. }
                    if next.canonical_inputs.active.as_ref().is_some_and(|active| {
                        active.logical_turn_id == logical_turn_id
                            && active.submission_ids.first() == Some(&attempt.submission_id)
                    }) && next.header.last_thread_id.as_deref()
                        == Some(report.correlation.thread_id.as_str()) =>
                {
                    if let Some(turn) = report.correlation.native_turn_id.as_ref() {
                        next.header.phase = HarnessPhaseTag::TurnRunning;
                        next.header.last_turn_id = Some(turn.clone());
                    }
                }
                InputDeliveryFact::CrossingUnknown
                | InputDeliveryFact::Unknown
                | InputDeliveryFact::NotDispatched { .. }
                | InputDeliveryFact::Accepted { .. } => return frame(&carrier),
            }
            let persisted = persist_runtime_frame_tx(
                tx,
                &card,
                &runtime,
                &RuntimeExecutionFence::from_carrier(&carrier),
                header_value(&next)?,
            )
            .await?;
            crate::db::sqlite::session_set_harness_observation_runtime_tx(
                tx,
                &runtime,
                if next.header.phase == HarnessPhaseTag::TurnRunning {
                    calm_types::worker::WorkerSessionState::Running
                } else if next.header.phase == HarnessPhaseTag::IssuingTurn {
                    calm_types::worker::WorkerSessionState::TurnPending
                } else {
                    calm_types::worker::WorkerSessionState::Idle
                },
                next.header.last_thread_id.as_deref(),
                if next.header.phase == HarnessPhaseTag::TurnRunning {
                    next.header.last_turn_id.as_deref()
                } else {
                    None
                },
            )
            .await?;
            written(persisted)
        })
    })
    .await?;
    publish(inner, next).await;
    if let Some(done) = crate::harness::input_dispatch::complete_once(
        inner.repo.as_ref(),
        &inner.backend,
        inner.card_id.as_str(),
        &inner.worker_session_id,
    )
    .await?
    {
        publish_end(inner, done).await?;
    }
    Ok(())
}

fn require_head(
    carrier: &CurrentInputCarrier,
    expected: &RuntimeExecutionFence,
    message: &CanonicalPendingIdentity,
) -> Result<()> {
    if RuntimeExecutionFence::from_carrier(carrier) != *expected
        || carrier
            .checkpoint()
            .pending
            .first()
            .map(PendingInput::identity)
            != Some(message.clone())
    {
        return Err(CalmError::Conflict(
            "canonical execution or pending head changed".into(),
        ));
    }
    Ok(())
}
