//! `POST /api/cards/{id}/planner/rewind` (#1923): remove the conversation's latest turn and hand
//! its user input back for the composer. Conversation only: files the turn changed stay changed.

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::CardId;
use crate::model::HarnessInputSegment;
use crate::per_card_lock::lock_card;
use crate::routes::cards::card_runs_headless_harness;
use crate::state::RouteState;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RewindPlannerRequest {
    /// The turn to remove: the `turnId` of the conversation's latest response.
    pub turn_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RewindPlannerResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub turn_id: String,
    /// The removed turn's user input, prompt then accepted steers in order, each image once.
    pub input: Vec<HarnessInputSegment>,
}

/// Same card gate as `/planner/input`, without its lazy recovery: a dormant harness has no turn
/// that could be rewound safely, so it is the same 409 `planner_harness_dormant` as `/interrupt`.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/rewind",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = RewindPlannerRequest,
    responses(
        (status = 200, description = "The turn is removed; `input` is what its user sent", body = RewindPlannerResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "Nothing changed: no live session (`planner_harness_dormant`), or the reason (`conflict`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "The provider did not check the edit in time; nothing changed", body = ErrorBody),
    ),
)]
pub(crate) async fn rewind_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<RewindPlannerRequest>,
) -> Result<Json<RewindPlannerResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    // The lock send, reset and lazy recovery take, so none of them moves the runtime underneath.
    let _recovery_guard = lock_card(&s.planner_recovery_locks, card.id.as_str()).await;
    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no live planner harness session for card {id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;
    let rewound = harness.rewind_turn(body.turn_id).await?;
    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        worker_session_id = %runtime.id,
        turn_id = %rewound.turn_id,
        "planner harness turn rewound"
    );
    Ok(Json(RewindPlannerResponse {
        card_id: card.id,
        turn_id: rewound.turn_id,
        input: rewound.input,
    }))
}
