//! `PUT /api/cards/{id}/planner/permission-mode` — the only writer of a Planner card's permission
//! mode (#2348). A person's choice: an agent, the Planner itself included, is refused, so a Planner
//! cannot raise its own permissions. Takes effect from the Planner's next turn.

use axum::extract::State;
pub use calm_types::harness::SetPlannerPermissionModeResponse;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::planner_permission_mode_set_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::extract::{Json, JsonBody, Path};
use crate::planner_permission_mode::{PlannerPermissionMode, card_has_permission_mode};
use crate::routes::cards::card_scope;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::RouteState;

const ACTOR_SUBJECT: &str = "planner permission mode";
const ACTOR_REDIRECT: &str = "What a Planner may do without asking is its person's choice; agents, \
     the Planner itself included, have no write path to it.";

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPlannerPermissionModeBody {
    /// The mode the Planner's next turns run with.
    pub permission_mode: PlannerPermissionMode,
}

#[utoipa::path(
    put,
    path = "/api/cards/{id}/planner/permission-mode",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = SetPlannerPermissionModeBody,
    responses(
        (status = 200, description = "Mode stored; the Planner's next turn runs with it", body = SetPlannerPermissionModeResponse),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a Planner card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 422, description = "`permission_mode` is missing or not a mode, or an unknown key is present", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn set_planner_permission_mode(
    State(s): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SetPlannerPermissionModeBody>,
) -> Result<Json<SetPlannerPermissionModeResponse>> {
    // The actor check runs first, so an agent probing card ids learns nothing from the status.
    require_rest_user_actor_for(&actor, ACTOR_SUBJECT, ACTOR_REDIRECT)?;

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let not_a_planner = format!("card {id} is not a Planner card");
    if !card_has_permission_mode(&card, role) {
        return Err(CalmError::Forbidden(not_a_planner));
    }

    let mode = body.permission_mode;
    let scope = card_scope(s.repo.as_ref(), card.id.clone(), card.track_id.clone()).await?;
    let card_id = card.id.clone();
    let (_card, _event_id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        {
            let card_id = card_id.to_string();
            move |tx| {
                Box::pin(async move {
                    // Judged again on the row this transaction writes.
                    let card = planner_permission_mode_set_tx(tx, &card_id, mode, |stored| {
                        if card_has_permission_mode(stored, role) {
                            Ok(())
                        } else {
                            Err(calm_truth::TruthError::Forbidden(not_a_planner))
                        }
                    })
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            }
        },
    )
    .await?;

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card_id,
        permission_mode = ?mode,
        "planner permission mode stored"
    );

    Ok(Json(SetPlannerPermissionModeResponse {
        card_id,
        permission_mode: mode,
    }))
}
