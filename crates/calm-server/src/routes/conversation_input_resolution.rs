//! A live Human session is the only input resolution authority.
use crate::auth::HumanInputSource;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use crate::ids::CardId;
use crate::state::RouteState;
use axum::{extract::State, http::HeaderMap};
use calm_types::conversation::{KeepConversationInputRequest, KeepConversationInputResult};

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
    state.events.emit_envelope(envelope);
    // Reconciliation failure cannot turn a committed receipt into a failed write.
    if let Some(handle) = state.harness.get(&result.carrier_id)
        && let Err(error) = handle.refresh_canonical_input().await
    {
        tracing::warn!(%error, "committed Human Keep awaits canonical refresh");
    }
    crate::operation::planner_start_fence::wake_canonical_human(state, card, source);
    Ok(Json(result))
}
