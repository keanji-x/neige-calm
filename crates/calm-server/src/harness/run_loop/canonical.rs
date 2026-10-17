//! Canonical inputs are read and claimed from the current carrier. This runtime
//! stores only transient exact-input sources and a committed read projection.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{RuntimeExecutionFence, current_input_carrier_tx};
use crate::harness::profile::input_carrier_binding_tx;
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use calm_types::conversation::MessageRevisionRef;
use std::collections::HashMap;
use calm_types::conversation::{AcceptedInputRef, InputDeliveryFact, PendingInput, SystemRenderContext};
use crate::db::sqlite::conversation::{CurrentInputCarrier, RuntimeFrameAfterWrite, persist_runtime_frame_tx, human_plan_for_pending_prefix_tx};

#[cfg(all(test, feature = "fixtures"))]
mod tests;

fn frame(carrier: &CurrentInputCarrier) -> Result<CommittedFrame> {
    Ok(CommittedFrame {
        snapshot: serde_json::from_value(carrier.snapshot().clone())?,
        fence: RuntimeExecutionFence::from_carrier(carrier),
    })
}

fn written(frame: RuntimeFrameAfterWrite) -> Result<CommittedFrame> {
    Ok(CommittedFrame {
        snapshot: serde_json::from_value(frame.snapshot)?,
        fence: frame.execution_fence,
    })
}

fn header_value(snapshot: &CanonicalHarnessSnapshot) -> Result<Value> {
    let mut value = serde_json::to_value(snapshot)?;
    value.as_object_mut().expect("declared canonical object")
        .remove(calm_types::conversation::CONVERSATION_INPUT_CHECKPOINT_KEY);
    Ok(value)
}

#[derive(Clone)]
pub(super) struct CommittedFrame {
    pub snapshot: CanonicalHarnessSnapshot,
    pub fence: RuntimeExecutionFence,
}

pub(super) struct CanonicalInputs {
    // This is an identity index, not an ordered queue or an admission cache.
    // Every claim/crossing still revalidates its actual issuer and current scope.
    sources: Mutex<HashMap<MessageRevisionRef, HumanInputSource>>,
    projection: Mutex<CommittedFrame>,
}

impl CanonicalInputs {
    pub fn new(frame: CommittedFrame) -> Self {
        Self {
            sources: Mutex::new(HashMap::new()),
            projection: Mutex::new(frame),
        }
    }

    pub async fn observe_source(&self, input: MessageRevisionRef, source: HumanInputSource) {
        self.sources.lock().await.insert(input, source);
    }

    pub async fn source(&self, input: &MessageRevisionRef) -> Option<HumanInputSource> {
        self.sources.lock().await.get(input).cloned()
    }

    pub async fn committed(&self) -> CommittedFrame {
        self.projection.lock().await.clone()
    }

    pub async fn publish_committed(&self, frame: CommittedFrame) {
        *self.projection.lock().await = frame;
    }
}

async fn read_current(inner: &Inner) -> Result<CommittedFrame> {
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    write_in_tx_typed(inner.repo.as_ref(), move |tx| Box::pin(async move {
        frame(&current_input_carrier_tx(tx, &card, &runtime).await?)
    })).await
}

/// This cache is published only from a returned, committed SQL result. Native
/// start/end notifications are triggers, never an alternate phase writer.
async fn publish(inner: &Inner, next: CommittedFrame) {
    let inputs = inner.inputs.canonical().expect("canonical consumer");
    let header = &next.snapshot.header;
    let mut state = inner.state.lock().await;
    let same_running = matches!(&*state, HarnessState::TurnRunning {turn_id,..}
        if header.phase == HarnessPhaseTag::TurnRunning && header.last_turn_id.as_ref() == Some(turn_id));
    if !same_running {
        *state = if header.phase == HarnessPhaseTag::TurnRunning {
            match &header.last_turn_id {
                Some(turn) => HarnessState::turn_running(turn.clone(), Instant::now()),
                None => HarnessState::Resumed { resumed_at: Instant::now() },
            }
        } else {
            state_from_runtime_header(header, &inner.inputs)
        };
    }
    drop(state);
    *inner.thread_id.write().await = header.last_thread_id.clone();
    *inner.last_turn_id.lock().await = header.last_turn_id.clone();
    *inner.turn_budget.lock().await = header.turn_budget.clone();
    *inner.last_phase.lock().await = header.phase;
    inputs.publish_committed(next).await;
}

/// Consume the one persisted pending order in the existing scheduler. Live
/// source entries carry no body, queue order, or reusable execution grant.
pub(super) async fn tick(inner: &Arc<Inner>) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Ok(());
    }
    if let Some(done) = crate::harness::input_dispatch::complete_once(
        inner.repo.as_ref(), &inner.backend, inner.card_id.as_str(), &inner.worker_session_id,
    ).await? {
        publish(inner, written(done)?).await;
    }
    let current = read_current(inner).await?;
    publish(inner, current.clone()).await;
    let checkpoint = &current.snapshot.canonical_inputs;
    let header = &current.snapshot.header;
    if inner.issuance_paused.load(Ordering::SeqCst)
        || provider_holds_issuance(inner).await
        || inner.issuance_retry_after.lock().await.is_some_and(|after| Instant::now() < after)
        || !inner.held_requests.is_empty()
        || header.last_thread_id.is_none()
        || header.pending_rewind.is_some()
        || header.interruption_intent.is_some()
        || header.wedged_reason.is_some()
        || checkpoint.active.is_some()
        || !matches!(header.phase, HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted)
    {
        return Ok(());
    }
    let Some(PendingInput::Human { message, .. }) = checkpoint.pending.first() else {
        return Ok(());
    };
    let inputs = inner.inputs.canonical().expect("canonical consumer");
    let Some(source) = inputs.source(message).await else { return Ok(()); };
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let expected = current.fence.clone();
    let message = message.clone();
    // Before model/workspace/config reads, validate the same source and exact
    // head through the shared admission owner. Claim and crossing repeat it.
    write_in_tx_typed(inner.repo.as_ref(), {
        let (card,runtime,source,message,expected) = (card.clone(),runtime.clone(),source.clone(),message.clone(),expected.clone());
        move |tx| Box::pin(async move {
            let carrier = current_input_carrier_tx(tx,&card,&runtime).await?;
            require_head(&carrier,&expected,&message)?;
            let plan = human_plan_for_pending_prefix_tx(tx,&carrier,std::num::NonZeroU32::new(1).expect("one input"),vec![]).await?;
            crate::conversation_inputs::authorize_human_execution_tx(tx,&source,&carrier,&plan).await?;
            Ok(())
        })
    }).await?;
    let resolved = async {
        let budget = budget::sample(inner).await?;
        let selection = inner.backend.resolve_selection(inner.as_ref()).await?;
        let permission = turn_permission::resolve(inner).await?;
        Ok::<_,IssuanceRefusal>((budget,selection,permission))
    }.await;
    let (budget,selection,permission) = match resolved {
        Ok(resolved) => resolved,
        Err(refusal) => { apply_refusal(inner,&refusal).await; return Ok(()); }
    };
    let mut contexts = vec![];
    if let Some(budget) = budget {
        contexts.push(SystemRenderContext::new(HarnessInputPresentation::SystemPlannerTurnBudget,budget.initial_input())
            .map_err(|error|CalmError::Conflict(error.into()))?);
    }
    let (claimed,claimed_frame) = write_in_tx_typed(inner.repo.as_ref(), {
        let source = source.clone();
        move |tx| Box::pin(async move {
            let mut carrier = current_input_carrier_tx(tx,&card,&runtime).await?;
            require_head(&carrier,&expected,&message)?;
            let plan = human_plan_for_pending_prefix_tx(tx,&carrier,std::num::NonZeroU32::new(1).expect("one input"),contexts).await?;
            let claimed = crate::conversation_inputs::claim_human_initial_tx(tx,&source,&mut carrier,plan).await?;
            let mut next: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
            next.header.turn_budget = match budget {
                Some(budget) => budget::FrozenBudget::Planner {
                    budget,
                    start_attempt: QueueEntryId::from_wire(claimed.submission_id.as_str().into()),
                    warning_attempted:false,
                },
                None => budget::FrozenBudget::NotApplicable,
            };
            let persisted = persist_runtime_frame_tx(tx,&card,&runtime,&RuntimeExecutionFence::from_carrier(&carrier),header_value(&next)?).await?;
            Ok((claimed,written(persisted)?))
        })
    }).await?;
    publish(inner,claimed_frame).await;
    let committed = crate::harness::input_dispatch::initial(
        inner.repo.as_ref(),&source,inner.card_id.as_str(),&inner.worker_session_id,
        &claimed.submission_id,&inner.backend,&selection,permission,None,
    ).await?;
    if matches!(committed.report.delivery(), crate::harness::backend::InputDelivery::NotDispatched(_)) {
        // Only the settled proof released input. Pacing is independent of that
        // proof and must not spin the scheduler at its tick frequency.
        *inner.issuance_retry_after.lock().await = Some(Instant::now() + TRANSIENT_RETRY_DELAY);
    }
    // Read the exact committed checkpoint, not Result::Ok/Err or an event's
    // apparent status. Unknown with no known execution remains Issuing.
    let attempt = committed.report.attempt().clone();
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let next = write_in_tx_typed(inner.repo.as_ref(),move |tx| Box::pin(async move {
        let carrier = current_input_carrier_tx(tx,&card,&runtime).await?;
        let report = carrier.checkpoint().attempt(&attempt.submission_id)
            .filter(|report|report.attempt == attempt)
            .ok_or_else(||CalmError::Conflict("committed dispatch report changed".into()))?;
        let mut next: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
        if next.header.phase != HarnessPhaseTag::IssuingTurn { return frame(&carrier); }
        match &report.delivery {
            InputDeliveryFact::NotDispatched { .. } if next.canonical_inputs.active.is_none() => {
                next.header.phase = HarnessPhaseTag::Idle;
            }
            InputDeliveryFact::Accepted { .. } | InputDeliveryFact::Unknown => {
                let turn = report.correlation.native_turn_id.as_ref().or(report.correlation.local_turn_id.as_ref());
                if let Some(turn) = turn {
                    next.header.phase = HarnessPhaseTag::TurnRunning;
                    next.header.last_turn_id = Some(turn.clone());
                }
            }
            _ => return frame(&carrier),
        }
        let persisted = persist_runtime_frame_tx(tx,&card,&runtime,&RuntimeExecutionFence::from_carrier(&carrier),header_value(&next)?).await?;
        crate::db::sqlite::session_set_harness_observation_runtime_tx(tx,&runtime,
            match next.header.phase {
                HarnessPhaseTag::TurnRunning => calm_types::worker::WorkerSessionState::Running,
                HarnessPhaseTag::IssuingTurn => calm_types::worker::WorkerSessionState::TurnPending,
                _ => calm_types::worker::WorkerSessionState::Idle,
            },
            next.header.last_thread_id.as_deref(),
            if next.header.phase == HarnessPhaseTag::TurnRunning {next.header.last_turn_id.as_deref()} else {None},
        ).await?;
        written(persisted)
    })).await?;
    publish(inner,next).await;
    if let Some(done) = crate::harness::input_dispatch::complete_once(inner.repo.as_ref(),&inner.backend,inner.card_id.as_str(),&inner.worker_session_id).await? {
        publish(inner,written(done)?).await;
    }
    Ok(())
}

fn require_head(carrier:&CurrentInputCarrier,expected:&RuntimeExecutionFence,message:&MessageRevisionRef)->Result<()> {
    if RuntimeExecutionFence::from_carrier(carrier) != *expected
        || carrier.checkpoint().pending.first().map(PendingInput::input_ref) != Some(AcceptedInputRef::Human { message:message.clone() })
    {
        return Err(CalmError::Conflict("canonical execution or pending head changed".into()));
    }
    Ok(())
}

pub(super) async fn on_notification(inner:&Arc<Inner>,live:&LiveReplyWriter,kind:PlannerEventKind)->Result<()> {
    match kind {
        PlannerEventKind::ReplyDelta {turn_id,item_id,delta} => live.delta(&turn_id,&item_id,&delta),
        PlannerEventKind::TurnStarted {..} | PlannerEventKind::TurnCompleted {..}
        | PlannerEventKind::TurnAborted {..} | PlannerEventKind::ThreadIdle => {
            if let Some(done) = crate::harness::input_dispatch::complete_once(inner.repo.as_ref(),&inner.backend,inner.card_id.as_str(),&inner.worker_session_id).await? {
                publish(inner,written(done)?).await;
            }
        }
        // Display/usage handling is added through the existing item owner; no
        // native echo can manufacture a second human message or phase writer.
        _ => {}
    }
    Ok(())
}

/// The supplied snapshot selected a contract, not a reusable execution fence.
/// Read the actual current carrier before the loop consumes any commands or
/// native events. No authorization, native readiness or source is fabricated.
pub(super) async fn load_current(params: &PlannerHarnessParams) -> Result<CommittedFrame> {
    let card = params.card_id.to_string();
    let track = params.track_id.to_string();
    let runtime = params.worker_session_id.clone();
    let supplied_thread = params.thread_id.clone();
    let provider = params.backend.provider();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
            let snapshot: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone()).map_err(|_| {
                    CalmError::Conflict("current canonical carrier has an invalid frame".into())
                })?;
            let (binding, current) = input_carrier_binding_tx(tx, &carrier).await?;
            if current.track_id.as_str() != track
                || binding.provider != provider
                || current.thread_id != snapshot.header.last_thread_id
                || supplied_thread
                    .as_ref()
                    .is_some_and(|thread| current.thread_id.as_ref() != Some(thread))
            {
                return Err(CalmError::Conflict(
                    "canonical runtime parameters differ from its current binding".into(),
                ));
            }
            if snapshot.header.phase == HarnessPhaseTag::TurnCompleted
                && snapshot
                    .header
                    .last_turn_id
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err(CalmError::Conflict(
                    "completed canonical frame has no exact original turn".into(),
                ));
            }
            Ok(CommittedFrame {
                snapshot,
                fence: RuntimeExecutionFence::from_carrier(&carrier),
            })
        })
    })
    .await
}
