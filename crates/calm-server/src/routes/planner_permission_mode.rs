//! `PUT /api/cards/{id}/planner/permission-mode` edits a bound Planner or Assistant's
//! approval choice. Only the person may write it; agents cannot raise their own mode.
//! New issuance uses the stored choice; running turns and frozen retries retain theirs.

use axum::extract::State;
pub use calm_types::harness::SetPlannerPermissionModeResponse;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::HumanInputSource;
use crate::db::sqlite::planner_permission_mode_set_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::extract::{Json, JsonBody, Path};
use crate::planner_permission_mode::{PlannerPermissionMode, card_has_permission_mode};
use crate::routes::cards::card_scope;
use crate::state::RouteState;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPlannerPermissionModeBody {
    /// The mode newly issued conversation turns run with.
    pub permission_mode: PlannerPermissionMode,
}

#[utoipa::path(
    put,
    path = "/api/cards/{id}/planner/permission-mode",
    tag = "cards",
    params(("id" = String, Path, description = "Planner or Assistant card id")),
    request_body = SetPlannerPermissionModeBody,
    responses(
        (status = 200, description = "Mode stored for newly issued conversation turns", body = SetPlannerPermissionModeResponse),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a bound Planner or Assistant card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 422, description = "`permission_mode` is missing or not a mode, or an unknown key is present", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn set_planner_permission_mode(
    State(s): State<RouteState>,
    source: HumanInputSource,
    share: Option<axum::Extension<crate::share_access::scope::ShareAccessScope>>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SetPlannerPermissionModeBody>,
) -> Result<Json<SetPlannerPermissionModeResponse>> {
    // The actor check runs first, so an agent probing card ids learns nothing from the status.
    crate::planner_permission_mode::require_explicit_selection(&actor, &source, share.is_some())?;

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let ineligible = format!("card {id} is not a bound Planner or Assistant conversation");
    if !card_has_permission_mode(&card, role) {
        return Err(CalmError::Forbidden(ineligible));
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
                    // Judge the persisted role and card in the same writer transaction.
                    let role = crate::db::sqlite::card_execution_shape_tx(tx, &card_id)
                        .await?
                        .role;
                    let card = planner_permission_mode_set_tx(tx, &card_id, mode, |stored| {
                        if card_has_permission_mode(stored, role) {
                            Ok(())
                        } else {
                            Err(calm_truth::TruthError::Forbidden(ineligible))
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
