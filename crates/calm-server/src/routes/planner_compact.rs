//! Human-only manual context compaction. The harness owns admission and sequencing.
use crate::extract::{Json, Path};
use crate::routes::{
    planner_cards::card_runs_headless_harness, track_report_blocks::require_rest_user_actor_for,
};
use crate::{
    actor::Actor,
    error::{CalmError, ErrorBody, Result},
    ids::CardId,
    state::RouteState,
};
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct CompactPlannerResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// Submission confirmed; the harness remains busy until the provider completes it.
    pub started: bool,
}

#[utoipa::path(post, path = "/api/cards/{id}/planner/compact", tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Context compaction submitted", body = CompactPlannerResponse),
        (status = 400, description = "Invalid path or provider does not support manual compaction", body = ErrorBody),
        (status = 403, description = "Human user or harness-backed card required", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "Conversation is busy or unavailable", body = ErrorBody),
        (status = 500, description = "Compaction submission failed", body = ErrorBody),
        (status = 503, description = "Harness command queue is full", body = ErrorBody),
    ))]
pub(crate) async fn compact_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<CompactPlannerResponse>> {
    require_rest_user_actor_for(
        &actor,
        "context compaction",
        "Only the person may compact their conversation.",
    )?;
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
        return Err(CalmError::Forbidden(
            "This card has no planner conversation.".into(),
        ));
    }
    let dormant =
        || CalmError::PlannerHarnessDormant("No live conversation is available to compact.".into());
    let runtime = s
        .repo
        .session_projection_active_for_card(&id)
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;
    match harness.compact().await {
        Err(CalmError::CodexRefused(_)) => {
            return Err(CalmError::BadRequest(
                "Codex refused context compaction; the conversation was not changed.".into(),
            ));
        }
        result => result?,
    }
    Ok(Json(CompactPlannerResponse {
        card_id: card.id,
        worker_session_id: runtime.id,
        started: true,
    }))
}
