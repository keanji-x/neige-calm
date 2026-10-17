//! `GET /api/cards/{id}/harness/live`: the reply text the running turn has streamed and not stored
//! yet (#1923 S2). Mounted by the cards router, which owns `/api/cards/{id}/**`.

use crate::error::CalmError;
use crate::extract::{Json, Path, Query};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use calm_types::conversation_display::ConversationLiveReplies;
use calm_types::harness::HarnessLiveReplies;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum HarnessLiveReadResponse {
    Historical(HarnessLiveReplies),
    Conversation(ConversationLiveReplies),
}

use crate::error::{ErrorBody, Result};
use crate::routes::cards::harness_card;
use crate::state::RouteState;

#[utoipa::path(
    get,
    path = "/api/cards/{id}/harness/live",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id"),super::conversation_run::RunReadQuery),
    responses(
        (status = 200, description = "Reply text streamed in the running turn and not stored yet", body = HarnessLiveReadResponse),
        (status = 401, description = "A verified session is required", body = ErrorBody),
        (status = 403, description = "Current Area access is required or card is not a harness card", body = ErrorBody),
        (status = 409, description = "Canonical live replies require the negotiated conversation contract", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_harness_live(
    State(s): State<RouteState>,
    principal: crate::auth::Principal,
    Path(id): Path<String>,
    Query(query): Query<super::conversation_run::RunReadQuery>,
) -> Result<Response> {
    let scope =
        super::conversation_read::ReadScope::check(s.repo.as_ref(), &id, &principal).await?;
    let card = harness_card(&s, &id).await?;
    let frame = super::conversation_read::current_frame(s.repo.as_ref(), &id).await?;
    let replies = if query.contract.is_some() {
        HarnessLiveReadResponse::Conversation(match frame.runtime.as_ref() {
            Some(runtime) => s
                .harness
                .live_replies()
                .read_conversation(&card.id, &runtime.id),
            None => ConversationLiveReplies {
                contract:
                    calm_types::conversation_display::ConversationReadContract::ConversationV1,
                turn: None,
            },
        })
    } else {
        if frame.canonical.is_some() {
            return Err(CalmError::Conflict(
                "canonical live replies require contract=conversation_v1".into(),
            ));
        }
        HarnessLiveReadResponse::Historical(s.harness.live_replies().read(&card.id))
    };
    scope.publish(s.repo.as_ref(), &principal).await?;
    Ok(Json(replies).into_response())
}
