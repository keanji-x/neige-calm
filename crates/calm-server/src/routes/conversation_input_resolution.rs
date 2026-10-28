//! A live Human session is the only input resolution authority.
use crate::auth::HumanInputSource;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use crate::ids::CardId;
use crate::state::RouteState;
use axum::{extract::State, http::HeaderMap};
use calm_types::conversation::{
    KeepConversationInputRequest, KeepConversationInputResult, RetryConversationInputRequest,
    RetryConversationInputResult,
};

#[utoipa::path(post, path = "/api/cards/{id}/conversation/input/keep", tag = "cards",
    params(("id" = String, Path), ("Idempotency-Key" = String, Header)),
    request_body = KeepConversationInputRequest,
    responses((status = 200, body = KeepConversationInputResult),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn keep_conversation_input(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path(card_id): Path<String>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<KeepConversationInputRequest>,
) -> Result<Json<KeepConversationInputResult>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::IdempotencyKeyInvalid("Keep requires an Idempotency-Key".into())
    })?;
    let card = CardId::from(card_id.clone());
    let fence = crate::operation::planner_start_fence::CardStartFence::lock(
        &state.planner_recovery_locks,
        &state.repo,
        &state.operation_runtime,
        &card,
    )
    .await;
    let (result, envelope) = crate::conversation_inputs::resolution::keep(
        state.repo.as_ref(),
        source.clone(),
        card_id,
        key,
        request,
    )
    .await?;
    drop(fence);
    refresh_and_wake(state, card, source, &result.carrier_id, envelope).await;
    Ok(Json(result))
}

#[utoipa::path(post, path = "/api/cards/{id}/conversation/input/retry", tag = "cards",
    params(("id" = String, Path), ("Idempotency-Key" = String, Header)),
    request_body = RetryConversationInputRequest,
    responses((status = 200, body = RetryConversationInputResult),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn retry_conversation_input(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path(card_id): Path<String>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<RetryConversationInputRequest>,
) -> Result<Json<RetryConversationInputResult>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::IdempotencyKeyInvalid("Retry requires an Idempotency-Key".into())
    })?;
    let card = CardId::from(card_id.clone());
    let fence = crate::operation::planner_start_fence::CardStartFence::lock(
        &state.planner_recovery_locks,
        &state.repo,
        &state.operation_runtime,
        &card,
    )
    .await;
    let (result, envelope) = crate::conversation_inputs::resolution::retry(
        state.repo.as_ref(),
        source.clone(),
        card_id,
        key,
        request,
    )
    .await?;
    drop(fence);
    refresh_and_wake(state, card, source, &result.carrier_id, envelope).await;
    Ok(Json(result))
}

/// Publication and wake are best effort after the one committed receipt.
async fn refresh_and_wake(
    state: RouteState,
    card: CardId,
    source: HumanInputSource,
    carrier: &String,
    envelope: crate::event::BroadcastEnvelope,
) {
    state.events.emit_envelope(envelope);
    if let Some(handle) = state.harness.get(carrier)
        && let Err(error) = handle.refresh_canonical_input().await
    {
        tracing::warn!(%error, "committed Human resolution awaits canonical refresh");
    }
    crate::operation::planner_start_fence::wake_canonical_human(state, card, source).await;
}
