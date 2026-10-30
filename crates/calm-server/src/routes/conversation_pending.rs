//! Canonical pending controls. HTTP identities never become execution evidence.
use super::planner_input::{PlannerInputMutationResponse, PlannerInputStaleBody};
use crate::auth::HumanInputSource;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use crate::state::RouteState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calm_truth::db::sqlite::conversation::{PendingHumanChange, PendingHumanWrite};
use calm_types::conversation::{ConversationMessageId, MessageRevisionRef};
use calm_types::conversation_controls::*;

fn reference(message: String, revision: u32) -> Result<MessageRevisionRef> {
    Ok(MessageRevisionRef {
        message_id: ConversationMessageId::parse(&message)
            .map_err(|e| CalmError::BadRequest(e.to_string()))?,
        revision: std::num::NonZeroU32::new(revision)
            .ok_or_else(|| CalmError::BadRequest("message revision must be positive".into()))?,
    })
}

#[utoipa::path(patch, path="/api/cards/{id}/conversation/messages/{message_id}", tag="cards",
    params(("id"=String,Path),("message_id"=String,Path)), request_body=EditQueuedMessageRequest,
    responses((status=200,body=PlannerInputMutationResponse),(status=400,body=ErrorBody),
        (status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=PlannerInputStaleBody)))]
pub(crate) async fn edit(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path((card, message)): Path<(String, String)>,
    JsonBody(request): JsonBody<EditQueuedMessageRequest>,
) -> Result<Response> {
    if request.text.chars().count() > super::planner_cards::MAX_PLANNER_INPUT_CHARS {
        return Err(CalmError::BadRequest(
            "message exceeds the text limit".into(),
        ));
    }
    mutate(
        state,
        source,
        card,
        request.carrier_id,
        reference(message, request.if_revision)?,
        PendingHumanChange::Edit { text: request.text },
    )
    .await
}

#[utoipa::path(delete, path="/api/cards/{id}/conversation/messages/{message_id}", tag="cards",
    params(("id"=String,Path),("message_id"=String,Path)), request_body=QueuedMessageRequest,
    responses((status=200,body=PlannerInputMutationResponse),(status=400,body=ErrorBody),
        (status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=PlannerInputStaleBody)))]
pub(crate) async fn withdraw(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path((card, message)): Path<(String, String)>,
    JsonBody(request): JsonBody<QueuedMessageRequest>,
) -> Result<Response> {
    mutate(
        state,
        source,
        card,
        request.carrier_id,
        reference(message, request.if_revision)?,
        PendingHumanChange::Withdraw,
    )
    .await
}

async fn mutate(
    state: RouteState,
    source: HumanInputSource,
    card: String,
    worker: String,
    expected: MessageRevisionRef,
    change: PendingHumanChange,
) -> Result<Response> {
    let card_id = crate::ids::CardId::from(card.clone());
    let _fence = crate::operation::planner_start_fence::CardStartFence::lock(
        &state.planner_recovery_locks,
        &state.repo,
        &state.operation_runtime,
        &card_id,
    )
    .await;
    let (result, event, track) = crate::conversation_inputs::pending_controls::mutate(
        state.repo.as_ref(),
        source.clone(),
        card,
        worker.clone(),
        expected.clone(),
        change,
    )
    .await?;
    match result {
        PendingHumanWrite::Applied {
            previous,
            current,
            payload,
        } => {
            let sources = state.harness.canonical_sources(&worker, &track);
            if let Some(current) = &current {
                sources.observe(current.clone().into(), source.clone().into());
            }
            sources.consumed(&[previous.into()]);
            if let Some(event) = event {
                state.events.emit_envelope(event);
            }
            if let Some(handle) = state.harness.get(&worker)
                && let Err(error) = handle.refresh_canonical_input().await
            {
                tracing::warn!(%error,"committed pending edit awaits runtime refresh");
            }
            let response = Json(PlannerInputMutationResponse {
                card_id: card_id.clone(),
                worker_session_id: worker,
                entry_id: expected.message_id.to_string(),
                rev: current
                    .as_ref()
                    .map_or(expected.revision.get(), |r| r.revision.get()),
                text: current.as_ref().map(|_| payload.text),
            })
            .into_response();
            drop(_fence);
            if current.is_some() {
                crate::operation::planner_start_fence::wake_canonical_human(state, card_id, source)
                    .await;
            }
            Ok(response)
        }
        PendingHumanWrite::Stale(current) => Ok((
            StatusCode::CONFLICT,
            Json(PlannerInputStaleBody {
                error: "This queued message changed; review its current version.".into(),
                code: "planner_input_stale".into(),
                entry_id: current.reference.message_id.to_string(),
                text: current.payload.text,
                rev: current.reference.revision.get(),
            }),
        )
            .into_response()),
        PendingHumanWrite::Gone => Err(CalmError::NotFound(
            "This message is no longer pending.".into(),
        )),
    }
}

#[utoipa::path(post, path="/api/cards/{id}/conversation/messages/{message_id}/steer", tag="cards",
    params(("id"=String,Path),("message_id"=String,Path)), request_body=SteerQueuedMessageRequest,
    responses((status=200,body=QueuedMessageSteerResult),(status=400,body=ErrorBody),
        (status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody)))]
pub(crate) async fn steer(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path((card, message)): Path<(String, String)>,
    JsonBody(request): JsonBody<SteerQueuedMessageRequest>,
) -> Result<Json<QueuedMessageSteerResult>> {
    let principal = source.current_principal()?;
    super::conversation_read::ReadScope::check(state.repo.as_ref(), &card, &principal).await?;
    let handle = state
        .harness
        .get(&request.carrier_id)
        .ok_or_else(|| CalmError::Conflict("conversation has no running native owner".into()))?;
    if handle.card_id().as_str() != card {
        return Err(CalmError::Conflict("conversation carrier changed".into()));
    }
    let result = handle
        .steer_canonical(
            source,
            reference(message, request.if_revision)?,
            request.turn_id,
        )
        .await?;
    if let Err(error) = handle.refresh_canonical_input().await {
        tracing::warn!(%error,"committed supplemental input awaits runtime refresh");
    }
    Ok(Json(result))
}
