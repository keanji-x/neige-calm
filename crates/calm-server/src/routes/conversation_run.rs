//! Adapt the existing current-state read to the persisted canonical header.
use super::conversation_read::{ReadScope, current_frame};
use super::planner_cards::GetPlannerRunResponse;
use crate::auth::Principal;
use crate::error::{CalmError, Result};
use crate::extract::Json;
use crate::harness::token_usage::planner_context_usage_from_header;
use crate::state::RouteState;
use axum::response::{IntoResponse, Response};
use calm_types::conversation::PendingInput;
use calm_types::conversation_display::*;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct RunReadQuery {
    pub contract: Option<ConversationReadContract>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum PlannerRunReadResponse {
    Historical(Box<GetPlannerRunResponse>),
    Conversation(Box<ConversationRunResponse>),
}

// Negotiated responses extend the old view, so both schemas may match: anyOf,
// not oneOf. Refer to the actual view schemas instead of copying their fields.
impl utoipa::PartialSchema for PlannerRunReadResponse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::AnyOfBuilder::new()
            .item(GetPlannerRunResponse::schema())
            .item(ConversationRunResponse::schema())
            .into()
    }
}
impl ToSchema for PlannerRunReadResponse {
    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        GetPlannerRunResponse::schemas(schemas);
        ConversationRunResponse::schemas(schemas);
    }
}

#[derive(Serialize, ToSchema)]
pub struct ConversationRunResponse {
    #[serde(flatten)]
    pub run: GetPlannerRunResponse,
    pub input_contract: ConversationInputContract,
    /// Current historical edit affordance; every write rechecks its actual source.
    pub history_replacement_supported: bool,
    /// Persisted terminal fact for the exact current execution after END consumption.
    #[schema(required = true, nullable = true)]
    pub terminal_outcome: Option<ConversationTerminalOutcome>,
    /// Confirmed timeout for this same current execution.
    #[schema(required = true, nullable = true)]
    pub terminal_timeout: Option<calm_types::harness::HarnessTimeoutEvidence>,
    /// Actual current worker row state. Canonical is always present; null means no current historical runtime.
    #[schema(required = true, nullable = true)]
    pub runtime_state: Option<calm_types::worker::WorkerSessionState>,
    pub pending_inputs: Vec<ConversationPendingInput>,
    #[schema(required = true, nullable = true)]
    pub active_input: Option<ConversationActiveInput>,
    #[schema(required = true, nullable = true)]
    pub native_turn_id: Option<String>,
}

pub(super) async fn read(
    s: RouteState,
    w: crate::state::WorkerState,
    principal: Principal,
    source: Option<crate::auth::HumanInputSource>,
    id: String,
    query: RunReadQuery,
) -> Result<Response> {
    let scope = ReadScope::check(s.repo.as_ref(), &id, &principal).await?;
    let card = super::cards::harness_card(&s, &id).await?;
    if card.track_id.as_str() != scope.track {
        return Err(CalmError::Forbidden("conversation moved".into()));
    }
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let frame = current_frame(s.repo.as_ref(), &id).await?;
    let runtime_fence = frame.runtime.clone();
    let snapshot_fence = frame.canonical.clone();
    let terminal_fence = frame.terminal_outcome.clone();
    let handle_fence = runtime_fence
        .as_ref()
        .and_then(|runtime| s.harness.get(&runtime.id));
    let response = if let Some(snapshot) = frame.canonical {
        if query.contract.is_none() {
            return Err(CalmError::Conflict(
                "canonical conversation requires contract=conversation_v1".into(),
            ));
        }
        let binding =
            crate::harness::profile::PlannerBinding::from_card(&card, role).ok_or_else(|| {
                CalmError::Forbidden(format!("card {id} is not a Planner conversation"))
            })?;
        let runtime = frame.runtime.as_ref().ok_or_else(|| {
            CalmError::Internal("canonical conversation has no current runtime fact".into())
        })?;
        let pending_inputs = snapshot
            .canonical_inputs
            .pending
            .iter()
            .map(|input| match input {
                PendingInput::ExplicitSuccessor {
                    retry_intent_id,
                    queue_position,
                    queued_at_ms,
                } => Ok(ConversationPendingInput::ExplicitSuccessor {
                    retry_intent_id: retry_intent_id.as_str().into(),
                    queue_position: *queue_position,
                    queued_at_ms: *queued_at_ms,
                }),
                PendingInput::Human {
                    message,
                    queue_position,
                    queued_at_ms,
                    ..
                } => Ok(ConversationPendingInput::Human {
                    message_id: message.message_id.as_str().into(),
                    revision: message.revision.get(),
                    queue_position: *queue_position,
                    queued_at_ms: *queued_at_ms,
                }),
                PendingInput::System {
                    observation_id,
                    queue_position,
                    queued_at_ms,
                    ..
                } => Ok(ConversationPendingInput::System {
                    observation_id: observation_id.as_str().into(),
                    queue_position: *queue_position,
                    queued_at_ms: *queued_at_ms,
                }),
            })
            .collect::<Result<Vec<_>>>()?;
        let header = snapshot.header;
        let issuance_block = match handle_fence.as_ref() {
            Some(handle) => handle.issuance_block().await,
            None => None,
        };
        // Only an outcome recorded for this exact execution may describe it.
        // A still-active or newly queued input cannot inherit the prior result.
        let terminal_outcome = (snapshot.canonical_inputs.active.is_none()
            && snapshot.canonical_inputs.pending.is_empty())
        .then(|| {
            frame
                .terminal_outcome
                .as_ref()
                .and_then(|row| row.display())
        })
        .flatten();
        let terminal_timeout = frame
            .terminal_outcome
            .as_ref()
            .and_then(|row| row.timeout());
        let active_input = crate::conversation_inputs::resolution::retry_read(
            s.repo.as_ref(),
            source.clone(),
            id.clone(),
            frame.active_input,
        )
        .await?;
        let recovery =
            crate::conversation_recovery::declaration(&s, source.clone(), &id, &runtime.id).await?;
        let recovery_block=recovery.as_ref().map(|recovery|match recovery {
            crate::harness::run_recovery::RunRecovery::AbandonSession{..}=>"The previous outcome is unknown. Start a fresh session to continue; history is kept.",
            crate::harness::run_recovery::RunRecovery::Held{reason:crate::harness::run_recovery::RunHeldReason::PendingInputs,..}=>"Queued observations belong to the old session. Resolve them before starting a fresh session.",
            crate::harness::run_recovery::RunRecovery::Held{cause:crate::harness::run_recovery::RunRecoveryCause::NativeCompletionPending,..}=>"Native completion is unconfirmed. Saved messages remain queued until reconciliation.",
            _=>"Execution remains held until safe recovery can be established. History is kept.",
        }.to_owned());
        let selection = crate::planner_model::CardModelSelection::from_payload(&card.payload)
            .unwrap_or_default();
        let usage = planner_context_usage_from_header(header.clone())
            .map(super::planner_cards::PlannerRunTokenUsage::from);
        let blocked_reason = header
            .wedged_reason
            .clone()
            .or(recovery_block)
            .or(issuance_block)
            .or_else(|| {
                runtime.state.is_terminal().then(|| {
                    if runtime.state == calm_types::worker::WorkerSessionState::Failed {
                "Execution is held after this session failed. Messages remain saved."
            } else {
                "Execution is held because this session is no longer active. Messages remain saved."
            }.into()
                })
            });
        Json(PlannerRunReadResponse::Conversation(Box::new(
            ConversationRunResponse {
                native_turn_id: (header.phase == crate::harness::HarnessPhaseTag::TurnRunning)
                    .then(|| header.last_turn_id.clone())
                    .flatten(),
                run: GetPlannerRunResponse {
                    card_id: card.id.clone(),
                    worker_session_id: Some(runtime.id.clone()),
                    phase: Some(header.phase),
                    model: selection.model,
                    reasoning_effort: selection.reasoning_effort,
                    permission_mode: crate::planner_permission_mode::card_has_permission_mode(
                        &card, role,
                    )
                    .then(|| crate::planner_permission_mode::shown_for_planner(&card)),
                    blocked_reason,
                    recovery,
                    token_usage: usage,
                    compact_outcome: header.compact_outcome,
                    context_usage_invalidated: header.context_usage_invalidated,
                    pending: vec![],
                    pending_overflow: 0,
                    // Canonical HTTP input admits ordinary text only (planner_input_send::human).
                    attachments_supported: false,
                    manual_compact_supported:
                        crate::harness::PlannerBackend::manual_compact_supported(&binding),
                    // Persisted native identity does not prove a monotonic running clock after reopen.
                    running_turn: None,
                },
                input_contract: ConversationInputContract::Canonical,
                history_replacement_supported: false,
                terminal_outcome,
                terminal_timeout,
                runtime_state: Some(runtime.state),
                pending_inputs,
                active_input,
            },
        )))
        .into_response()
    } else {
        let Json(run) =
            super::planner_cards::historical_planner_run(s.clone(), w, id.clone()).await?;
        if query.contract.is_some() {
            if run.worker_session_id.as_ref().is_some_and(|id| {
                runtime_fence
                    .as_ref()
                    .is_none_or(|runtime| runtime.id != *id)
            }) {
                return Err(CalmError::Conflict(
                    "conversation runtime changed while reading".into(),
                ));
            }
            let edit_handle = run.worker_session_id.as_ref().and(handle_fence.as_ref());
            let history_replacement_supported =
                historical_edit_capability(&s, &principal, source, &scope, edit_handle).await?;
            Json(PlannerRunReadResponse::Conversation(Box::new(
                ConversationRunResponse {
                    native_turn_id: run.running_turn.as_ref().map(|turn| turn.turn_id.clone()),
                    run,
                    input_contract: ConversationInputContract::Historical,
                    history_replacement_supported,
                    terminal_outcome: None,
                    terminal_timeout: None,
                    runtime_state: frame.runtime.map(|runtime| runtime.state),
                    pending_inputs: vec![],
                    active_input: None,
                },
            )))
            .into_response()
        } else {
            Json(PlannerRunReadResponse::Historical(Box::new(run))).into_response()
        }
    };
    scope.publish(s.repo.as_ref(), &principal).await?;
    if query.contract.is_some() {
        let published = current_frame(s.repo.as_ref(), &id).await?;
        let same_execution =
            published.canonical == snapshot_fence && published.terminal_outcome == terminal_fence;
        let current = published.runtime;
        let same_runtime = match (runtime_fence.as_ref(), current.as_ref()) {
            (Some(before), Some(now)) => before.id == now.id && before.state == now.state,
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        let current_handle = current
            .as_ref()
            .and_then(|runtime| s.harness.get(&runtime.id));
        let same_handle = match (handle_fence.as_ref(), current_handle.as_ref()) {
            (Some(before), Some(now)) => before.same_instance(now),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        if !same_runtime || !same_handle || !same_execution {
            return Err(CalmError::Conflict(
                "conversation runtime changed while reading".into(),
            ));
        }
    }
    Ok(response)
}

/// A display affordance consumes the actual effect owner's policy, never a role/name guess.
async fn historical_edit_capability(
    state: &RouteState,
    principal: &Principal,
    source: Option<crate::auth::HumanInputSource>,
    scope: &ReadScope,
    handle: Option<&crate::harness::PlannerHarness>,
) -> Result<bool> {
    let backend_supported = handle.is_some_and(|handle| handle.history_replacement_supported());
    let Some(source) = source.filter(|_| backend_supported) else {
        return Ok(false);
    };
    match source.current_principal() {
        Ok(current) if current.user_id == principal.user_id => {}
        Ok(_) | Err(CalmError::Unauthorized | CalmError::Forbidden(_)) => return Ok(false),
        Err(error) => return Err(error),
    }
    match crate::conversation_inputs::HistoricalHumanSource::new(source)
        .require_area(state.repo.as_ref(), scope.area.as_str())
        .await
    {
        Ok(()) => Ok(true),
        Err(CalmError::Unauthorized | CalmError::Forbidden(_)) => Ok(false),
        Err(error) => Err(error),
    }
}
