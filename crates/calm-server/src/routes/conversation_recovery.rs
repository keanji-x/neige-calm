//! Human HTTP adapter for the canonical recovery owner.
use crate::extract::{Json, JsonBody, Path};
use crate::{
    auth::HumanInputSource,
    error::{CalmError, ErrorBody, Result},
    state::{AppState, RouteState},
};
use axum::{Router, extract::State, http::HeaderMap, routing::post};
pub use calm_types::conversation::{AbandonConversationRequest, AbandonConversationResult};
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cards/{id}/conversation/abandon", post(abandon))
        .route("/api/cards/{id}/conversation/recover", post(recover))
}
#[utoipa::path(post,path="/api/cards/{id}/conversation/abandon",tag="cards",
 params(("id"=String,Path),("Idempotency-Key"=String,Header)),request_body=AbandonConversationRequest,
 responses((status=200,body=AbandonConversationResult),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody)))]
pub(crate) async fn abandon(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path(card): Path<String>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<AbandonConversationRequest>,
) -> Result<Json<AbandonConversationResult>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::IdempotencyKeyInvalid("Recovery requires an Idempotency-Key".into())
    })?;
    Ok(Json(
        crate::conversation_recovery::abandon(state, source, card, key, request).await?,
    ))
}

#[utoipa::path(post,path="/api/cards/{id}/conversation/recover",tag="cards",
 params(("id"=String,Path),("Idempotency-Key"=String,Header)),request_body=AbandonConversationRequest,
 responses((status=200,body=AbandonConversationResult),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn recover(
    State(state): State<RouteState>,
    source: HumanInputSource,
    Path(card): Path<String>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<AbandonConversationRequest>,
) -> Result<Json<AbandonConversationResult>> {
    let key = super::idempotency_key::parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::IdempotencyKeyInvalid("Recovery requires an Idempotency-Key".into())
    })?;
    Ok(Json(
        crate::conversation_recovery::recover(state, source, card, key, request).await?,
    ))
}
