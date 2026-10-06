//! `GET /api/cards/{id}/harness/live`: the reply text the running turn has streamed and not stored
//! yet (#1923 S2). Mounted by the cards router, which owns `/api/cards/{id}/**`.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use calm_types::harness::HarnessLiveReplies;

use crate::error::{ErrorBody, Result};
use crate::routes::cards::harness_card;
use crate::state::RouteState;

#[utoipa::path(
    get,
    path = "/api/cards/{id}/harness/live",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Reply text streamed in the running turn and not stored yet", body = HarnessLiveReplies),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_harness_live(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<HarnessLiveReplies>> {
    let card = harness_card(&s, &id).await?;
    Ok(Json(s.harness.live_replies().read(&card.id)))
}
