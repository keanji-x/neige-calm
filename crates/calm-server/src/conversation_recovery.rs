//! Canonical recovery owner. Stop never means END; abandonment retains old facts.
use crate::harness::backend::factory::PlannerFactory;
use crate::harness::{carrier_replacement::HeldCarrierOwner, registry::CarrierRetirementSlot};
use crate::mcp_server::transport::carrier_quiescence::ToolCallGuard;
use crate::{
    auth::HumanInputSource,
    error::{CalmError, Result},
    state::RouteState,
};
use calm_truth::{
    db::sqlite::{self, conversation::*},
    planner_binding::PlannerBinding,
};
use calm_types::conversation::{AbandonConversationRequest, AbandonConversationResult};

pub(crate) async fn abandon(
    state: RouteState,
    source: HumanInputSource,
    card: String,
    key: String,
    request: AbandonConversationRequest,
) -> Result<AbandonConversationResult> {
    // The recovery owner survives an HTTP waiter disappearing during stop or commit.
    tokio::spawn(async move { abandon_owned(state, source, card, key, request).await })
        .await
        .map_err(|_| CalmError::Internal("canonical recovery task failed".into()))?
}

async fn authorize(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source: &HumanInputSource,
    card: &str,
) -> Result<crate::auth::Principal> {
    let principal = source.current_principal()?;
    crate::conversation_inputs::authorize_human_tx(tx, &principal, card).await?;
    crate::conversation_inputs::authorize_human_resource_tx(tx, source, card).await?;
    Ok(principal)
}

struct Protected<'a> {
    source: &'a HumanInputSource,
    held: Option<&'a HeldCarrierOwner<'a>>,
    calls: &'a ToolCallGuard,
    proof: &'a crate::provider_execution::quiescence::Quiescence,
    request: &'a AbandonConversationRequest,
    mode: AbandonmentMode,
}
#[async_trait::async_trait]
impl CarrierReplacementGuard for Protected<'_> {
    async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> calm_truth::error::Result<()> {
        self.source
            .current_principal()
            .map_err(|_| calm_truth::error::TruthError::Conflict("Human source changed"))?;
        if let Some(held) = self.held {
            held.recheck()
                .map_err(|_| calm_truth::error::TruthError::Conflict("recovery owner changed"))?;
        }
        if self.calls.worker() != carrier.worker_session_id() || self.calls.require_idle().is_err()
        {
            return Err(calm_truth::error::TruthError::Conflict(
                "recovery has pending MCP calls",
            ));
        }
        let target = PlannerFactory::abandonment_target_tx(tx, carrier)
            .await
            .map_err(|_| calm_truth::error::TruthError::Conflict("recovery eligibility changed"))?;
        if !target.is_some_and(|target| target.request == *self.request && target.mode == self.mode)
        {
            return Err(calm_truth::error::TruthError::Conflict(
                "recovery eligibility changed",
            ));
        }
        self.proof
            .recheck_tx(tx)
            .await
            .map_err(|_| calm_truth::error::TruthError::Conflict("execution quiescence changed"))?;
        Ok(())
    }
}

async fn abandon_owned(
    s: RouteState,
    source: HumanInputSource,
    card: String,
    key: String,
    request: AbandonConversationRequest,
) -> Result<AbandonConversationResult> {
    let db = s
        .mcp_context
        .sqlite_db
        .clone()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let mut tx = sqlite::begin_immediate_tx(&db).await?;
    let principal = authorize(&mut tx, &source, &card).await?;
    if let Some(replay) =
        replay_abandonment_tx(&mut tx, &card, &principal.user_id, &key, &request).await?
    {
        source.current_principal()?;
        tx.rollback().await?;
        return Ok(replay);
    }
    let carrier = current_input_fact_carrier_tx(&mut tx, &card, &request.carrier_id).await?;
    if PlannerFactory::abandonment_target_tx(&mut tx, &carrier)
        .await?
        .map(|t| t.request)
        .as_ref()
        != Some(&request)
    {
        return Err(CalmError::Conflict(
            "Unknown recovery requires the current input; resolve worker-bound queued work first."
                .into(),
        ));
    }
    let (binding, row) =
        crate::harness::profile::input_carrier_contract_tx(&mut tx, &carrier).await?;
    let track = row.track_id.clone();
    tx.rollback().await?;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("canonical-abandon-before-push", &card).await;
    let _push = s.dispatcher.planner_switch_push_lock(&track).await;
    let mut replay_tx = sqlite::begin_immediate_tx(&db).await?;
    let principal = authorize(&mut replay_tx, &source, &card).await?;
    if let Some(replay) =
        replay_abandonment_tx(&mut replay_tx, &card, &principal.user_id, &key, &request).await?
    {
        source.current_principal()?;
        replay_tx.rollback().await?;
        return Ok(replay);
    }
    replay_tx.rollback().await?;
    let handle = s.harness.get(&request.carrier_id);
    let slot = s.harness.carrier_retirement_slot(&request.carrier_id);
    if matches!(slot, CarrierRetirementSlot::Reserved(_)) {
        return Err(CalmError::Conflict(
            "original carrier is being restored".into(),
        ));
    }
    let held = match &handle {
        Some(h) => Some(h.held_carrier_owner().await?),
        None => None,
    };
    let calls = s
        .mcp_context
        .tool_calls
        .fence(&request.carrier_id)
        .map_err(|_| CalmError::Conflict("Wait for pending tool calls before recovery".into()))?;
    let card_id = crate::ids::CardId::from(card.clone());
    let _start = crate::operation::planner_start_fence::CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        &card_id,
    )
    .await;
    let _drive = s.operation_runtime.lock_for_track_delete().await;
    let _track = crate::per_card_lock::lock_key(&s.track_delete_locks, track.as_str()).await;
    let mut tx = sqlite::begin_immediate_tx(&db).await?;
    authorize(&mut tx, &source, &card).await?;
    let captured = current_input_fact_carrier_tx(&mut tx, &card, &request.carrier_id).await?;
    let target = PlannerFactory::abandonment_target_tx(&mut tx, &captured)
        .await?
        .filter(|target| target.request == request)
        .ok_or_else(|| CalmError::Conflict("Recovery target changed".into()))?;
    let shape = sqlite::card_execution_shape_tx(&mut tx, &card).await?;
    let captured_binding = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
        .ok_or_else(|| CalmError::Conflict("recovery binding unsupported".into()))?;
    if captured_binding.provider != binding.provider
        || !s
            .harness
            .retirement_slot_is_current(&request.carrier_id, &slot)
    {
        return Err(CalmError::Conflict("recovery owner changed".into()));
    }
    crate::operation::planner_start_fence::CardStartFence::require_settled_tx(&mut tx, &card)
        .await?;
    crate::ask::require_no_open_carrier_asks_tx(&mut tx, &card).await?;
    let ingress =
        carrier_ingress_revision_tx(&mut tx, &card, track.as_str(), &request.carrier_id).await?;
    tx.rollback().await?;
    // No provider I/O while holding the SQL writer. The native/harness park stays held.
    let proof = PlannerFactory::new(
        std::sync::Arc::downgrade(&s.shared_codex_appserver),
        &s.claude_planner,
        &s.acp_planner,
    )
    .quiesce_for_abandonment(&binding.provider, &request.carrier_id, track.as_str())
    .await?;
    if let Some(held) = &held {
        held.recheck()?;
    }
    let owner = Protected {
        source: &source,
        held: held.as_ref(),
        calls: &calls,
        proof: &proof,
        request: &request,
        mode: target.mode,
    };
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("canonical-abandon-before-commit", &card).await;
    let mut tx = sqlite::begin_immediate_tx(&db).await?;
    let principal = authorize(&mut tx, &source, &card).await?;
    if !s
        .harness
        .retirement_slot_is_current(&request.carrier_id, &slot)
    {
        return Err(CalmError::Conflict("recovery owner changed".into()));
    }
    crate::operation::planner_start_fence::CardStartFence::require_settled_tx(&mut tx, &card)
        .await?;
    crate::ask::require_no_open_carrier_asks_tx(&mut tx, &card).await?;
    let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(captured.snapshot().clone())?;
    let replacement = abandon_unresolved_carrier_tx(
        &mut tx,
        ExpectedCarrier {
            card_id: card.clone(),
            track_id: track.to_string(),
            worker_session_id: request.carrier_id.clone(),
            original_snapshot: captured.original_snapshot().into(),
            execution: RuntimeExecutionFence::from_carrier(&captured),
            binding: captured_binding,
            accepted_prefix: frame.header.push_watermark,
            ingress,
            owner: &owner,
            revision: CallerRevision::Unversioned,
        },
        target.mode,
        &request,
        &principal.user_id,
        &key,
    )
    .await?;
    let area = conversation_area_tx(&mut tx, &card).await?;
    let scope = crate::event::EventScope::Card {
        card: card.clone().into(),
        track: track.clone(),
        area: area.id,
    };
    let event = crate::event::Event::HarnessPhaseChanged {
        worker_session_id: replacement.new_worker.id.to_string(),
        card_id: card.clone().into(),
        track_id: track.clone(),
        old_phase: frame.header.phase,
        new_phase: calm_types::harness::HarnessPhaseTag::Idle,
    };
    let event_id = sqlite::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::User,
        &scope,
        None,
        &event,
    )
    .await?;
    source.current_principal()?;
    tx.commit().await?;
    #[allow(deprecated)]
    s.harness
        .retire_and_publish_replacement(&replacement, &slot, s.write.role_cache())?;
    s.events.emit_envelope(crate::event::BroadcastEnvelope {
        id: event_id,
        event_version: crate::event::SYNC_EVENT_VERSION,
        actor: crate::ids::ActorId::User,
        scope,
        event,
    });
    // Fresh carrier stays dormant; only a subsequent authorized input may activate it.
    drop(_drive);
    drop(held);
    if let Err(error) = crate::harness::HarnessRegistry::cleanup_replacement(
        &replacement,
        slot,
        Some(&s.shared_codex_appserver),
    )
    .await
    {
        tracing::warn!(%error,"committed canonical recovery cleanup pending");
    }
    Ok(AbandonConversationResult {
        card_id: card,
        old_carrier_id: request.carrier_id,
        carrier_id: replacement.new_worker.id.to_string(),
    })
}

/// Read-only declaration; the explicit action still establishes quiescence at execution time.
pub(crate) async fn declaration(
    s: &RouteState,
    source: Option<HumanInputSource>,
    card: &str,
    worker: &str,
) -> Result<Option<crate::harness::run_recovery::RunRecovery>> {
    use crate::harness::run_recovery::{
        RunHeldReason as H, RunRecovery as R, RunRecoveryCause as C,
    };
    let (state, card, worker) = (s.clone(), card.to_owned(), worker.to_owned());
    crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            let (binding, _) =
                crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
            if binding.provider.protocol() == calm_types::runtime::AgentProtocol::Claude
                && crate::claude_planner::recovery::identity_unconfirmed_tx(tx, &worker).await?
            {
                let request = PlannerFactory::abandonment_target_tx(tx, &carrier)
                    .await?
                    .map(|t| t.request);
                let authorized = if let Some(source) = source {
                    match authorize(tx, &source, &card).await {
                        Ok(_) => true,
                        Err(CalmError::Unauthorized | CalmError::Forbidden(_)) => false,
                        Err(error) => return Err(error),
                    }
                } else {
                    false
                };
                return Ok(Some(
                    if let Some(request) = request.filter(|_| authorized) {
                        R::AbandonSession {
                            cause: C::UnknownOutcome,
                            request,
                        }
                    } else {
                        R::Held {
                            cause: C::UnknownOutcome,
                            reason: if carrier.checkpoint().pending.iter().any(|p| {
                                !matches!(p, calm_types::conversation::PendingInput::Human { .. })
                            }) {
                                H::PendingInputs
                            } else {
                                H::IdentityUnconfirmed
                            },
                        }
                    },
                ));
            }
            let Some(original) = original_input_execution_tx(tx, &carrier).await? else {
                return Ok(None);
            };
            let Some(fact) = carrier
                .checkpoint()
                .attempt(&original.proof.attempt().submission_id)
            else {
                return Ok(None);
            };
            match binding.provider.protocol() {
                calm_types::runtime::AgentProtocol::Acp
                    if matches!(
                        fact.delivery,
                        calm_types::conversation::InputDeliveryFact::Unknown
                    ) =>
                {
                    // Canonical Unknown also describes a live prompt awaiting its
                    // receipt. Only the journal's unresolved Unknown is recovery;
                    // Sending and Completed retain ordinary typed delivery feedback.
                    if let Some(receipt) = sqlite::acp_submission_get(
                        &mut **tx,
                        &worker,
                        fact.attempt.submission_id.as_str(),
                    )
                    .await?
                        && !matches!(receipt.state, sqlite::AcpSubmissionState::Unknown)
                    {
                        return Ok(None);
                    }
                    let request = PlannerFactory::abandonment_target_tx(tx, &carrier)
                        .await?
                        .map(|t| t.request);
                    let authorized = if let Some(source) = source {
                        match authorize(tx, &source, &card).await {
                            Ok(_) => true,
                            Err(CalmError::Unauthorized | CalmError::Forbidden(_)) => false,
                            Err(error) => return Err(error),
                        }
                    } else {
                        false
                    };
                    Ok(Some(
                        if let Some(request) = request.filter(|_| authorized) {
                            R::AbandonSession {
                                cause: C::UnknownOutcome,
                                request,
                            }
                        } else {
                            R::Held {
                                cause: C::UnknownOutcome,
                                reason: if carrier.checkpoint().pending.is_empty() {
                                    H::CleanupRequired
                                } else {
                                    H::PendingInputs
                                },
                            }
                        },
                    ))
                }
                calm_types::runtime::AgentProtocol::Codex
                    if matches!(
                        fact.delivery,
                        calm_types::conversation::InputDeliveryFact::Accepted { .. }
                    ) =>
                {
                    let active = state
                        .planner_start_adapter
                        .canonical_native_active_tx(tx, &worker, &original.correlation.thread_id)
                        .await?;
                    Ok((!active).then_some(R::Held {
                        cause: C::NativeCompletionPending,
                        reason: H::NativeCompletionUnavailable,
                    }))
                }
                _ => Ok(None),
            }
        })
    })
    .await
}
