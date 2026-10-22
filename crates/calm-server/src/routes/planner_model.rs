//! `PUT /api/cards/{id}/planner/model` — records which model a card's conversation
//! runs with. Both keys are required and `null` is a value; an unknown slug is
//! reported, not refused; an unsupported effort is moved to the model's default. One advice for
//! both providers (#1822 6′): only the catalog's source differs, Codex's `model/list` asked now or
//! the Claude CLI's list the availability check cached. Claude PUT and excluded historical creation
//! need readiness (400 otherwise); fresh blank managed creation is durably accepted and execution
//! waits. Without a catalog it stores model/effort unjudged (unknown_model=false, no adjustment).
//! With a catalog, a Claude entry declares no default effort, so an unsupported one is dropped,
//! and so is any effort sent with a model the Claude list does not carry.

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::card_update_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::extract::{Json, JsonBody, Path};
use crate::ids::CardId;
use crate::model::CardPatch;
use crate::operation::codex_adapter::card_payload_get_tx;
use crate::planner_model::CardModelSelection;
use crate::routes::cards::card_scope;
use crate::routes::planner_cards::card_runs_headless_harness;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::{CodexShellState, RouteState, WorkerState};

const ACTOR_SUBJECT: &str = "planner model selection";
const ACTOR_REDIRECT: &str = "Which model a person's conversation runs with is their own choice; agents have no write \
     path to it.";

/// Accept `null`, refuse absence: a `deserialize_with` makes the `Option<T>` field
/// required to serde while still accepting an explicit `null`.
fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPlannerModelBody {
    /// The model **slug** to run with (never a catalog entry's `id`; for a Claude Planner a
    /// `model` of its catalog, the Claude CLI's `value`), or `null` to follow the installation
    /// default (for a Claude Planner, the Claude CLI's own). `#[schema(required = true)]` is needed because utoipa
    /// derives optionality from the `Option<T>` alone and cannot see `deserialize_with`.
    #[schema(required = true)]
    #[serde(deserialize_with = "required_nullable")]
    pub model: Option<String>,
    /// The reasoning effort, or `null` to follow the default. Required. A bare string:
    /// codex accepts any non-empty effort. For a Claude Planner, one the chosen entry declares
    /// (for a `null` model, the CLI's `default` entry); any other is dropped (`effort_adjusted`).
    #[schema(required = true)]
    #[serde(deserialize_with = "required_nullable")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SetPlannerModelResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// The stored slug, echoed rather than assumed.
    pub model: Option<String>,
    /// The stored effort. Differs from the request when `effort_adjusted`.
    pub reasoning_effort: Option<String>,
    /// The requested effort was not supported by the chosen model and was moved to that
    /// model's own default (for a Claude Planner, which declares none, dropped to `null`).
    pub effort_adjusted: bool,
    /// The slug is not in the catalog the provider currently reports. A hint, not a refusal;
    /// always `false` when the catalog could not be read.
    pub unknown_model: bool,
}

#[utoipa::path(
    put,
    path = "/api/cards/{id}/planner/model",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = SetPlannerModelBody,
    responses(
        (status = 200, description = "Selection stored. `effort_adjusted` and `unknown_model` report what the catalog said about it; neither is an error", body = SetPlannerModelResponse),
        (status = 400, description = "A Claude Planner card while Claude is not ready, with the `GET /api/agent-providers` reason; nothing is stored", body = ErrorBody),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 422, description = "`model` or `reasoning_effort` is missing from the body, or an unknown key is present. Both keys are required; `null` is how the default is chosen", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn set_planner_model(
    State(s): State<RouteState>,
    State(codex): State<CodexShellState>,
    State(workers): State<WorkerState>,
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<SetPlannerModelBody>,
) -> Result<(StatusCode, Json<SetPlannerModelResponse>)> {
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
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    let provider = crate::harness::profile::checked_card_binding(s.repo.as_ref(), &card, role)
        .await?
        .provider;

    let SetPlannerModelBody {
        model,
        reasoning_effort,
    } = body;
    let advice = crate::planner_model::acquisition::CatalogAccess::new(
        &s.provider_availability,
        &s.claude_planner,
        &s.acp_planner,
        &codex.shared_codex_appserver,
    )
    .model_write(
        &provider,
        &id,
        model.as_deref(),
        reasoning_effort.as_deref(),
    )
    .await?;
    let stored_effort = match &advice.adjustment {
        Some(adjustment) => adjustment.to.clone(),
        None => reasoning_effort.clone(),
    };

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
            let model = model.clone();
            let stored_effort = stored_effort.clone();
            let card_id = card_id.to_string();
            move |tx| {
                Box::pin(async move {
                    // Read-modify-write inside the transaction: the payload column is replaced wholesale.
                    let mut payload = card_payload_get_tx(tx, &card_id).await?;
                    let map = payload.as_object_mut().ok_or_else(|| {
                        CalmError::Internal(format!(
                            "planner card {card_id} payload is not a JSON object"
                        ))
                    })?;
                    CardModelSelection::apply_to_payload(
                        map,
                        model.as_deref(),
                        stored_effort.as_deref(),
                    );
                    let card = card_update_tx(
                        tx,
                        &card_id,
                        CardPatch {
                            title: None,
                            kind: None,
                            sort: None,
                            payload: Some(payload),
                            deletable: None,
                        },
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            }
        },
    )
    .await?;

    // A model was just picked, so a harness waiting out its refusal interval must stop
    // waiting. Best-effort: no live harness means nothing was waiting.
    if let Ok(Some(runtime)) = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await
        && let Some(harness) = workers.harness.get(&runtime.id)
    {
        harness.retry_issuance_now().await;
    }

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card_id,
        model = ?model,
        reasoning_effort = ?stored_effort,
        effort_adjusted = advice.adjustment.is_some(),
        unknown_model = advice.unknown_model,
        "planner conversation model selection stored"
    );

    Ok((
        StatusCode::OK,
        Json(SetPlannerModelResponse {
            card_id,
            model,
            reasoning_effort: stored_effort,
            effort_adjusted: advice.adjustment.is_some(),
            unknown_model: advice.unknown_model,
        }),
    ))
}
