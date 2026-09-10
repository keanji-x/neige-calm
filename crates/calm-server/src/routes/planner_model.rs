//! #1505 S4-3 — `PUT /api/cards/{id}/planner/model`.
//!
//! The write port for "which model does this conversation run with". Its read
//! twin is `GET /api/models`, which lists what can be chosen and which default
//! this installation follows; this one records a card's answer.
//!
//! Four decisions are worth reading before the code.
//!
//! **Both keys are required, and `null` is a value.** This is a PUT, so the
//! body states the whole selection: `{"model": null, "reasoning_effort":
//! null}` means "follow the installation default for both" and a body that
//! omits `reasoning_effort` is refused rather than read as that. A partial
//! update would be a PATCH, and inventing one here would give the request
//! three states (absent / null / value) where the stored selection only has
//! two — the third would have to be resolved by a rule nobody wrote down.
//! Plain `Option<String>` fields would have accepted the omission silently:
//! serde treats a syntactically-`Option` field as optional unless something
//! stops it, which is what [`required_nullable`] is for.
//!
//! **An unknown slug is reported, not refused.** `unknown_model` says the slug
//! is not in the catalog codex just gave us. It is deliberately not a 400: the
//! catalog can be the five bundled presets (a signed-out daemon), and refusing
//! then would block a model this account can actually run. The person is told
//! and keeps the choice.
//!
//! **An effort the chosen model does not support is moved, and said so.** It
//! lands on that model's own `defaultReasoningEffort` and the response carries
//! `effort_adjusted: true`. Silently storing an unsupported pair, or silently
//! correcting it, both leave the UI showing something that is not what will
//! run.
//!
//! **The write reads its own base inside the transaction.** `CardPatch`
//! replaces the whole `payload` column, so a base captured before the codex
//! round trips above would drop every key another writer added in between —
//! the exact defect #1505 S4-1 fixed in the harness-start adapter. See
//! `card_apply_harness_start_payload_tx`.

use axum::extract::{Path, State};
use axum::{Json, http::StatusCode};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::card_update_tx;
use crate::db::write_with_event_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::ids::CardId;
use crate::model::CardPatch;
use crate::operation::codex_adapter::card_payload_get_tx;
use crate::planner_model::CardModelSelection;
use crate::routes::cards::{card_runs_headless_harness, card_scope};
use crate::routes::models::CODEX_READ_TIMEOUT;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::{CodexShellState, RouteState, WorkerState};

const ACTOR_SUBJECT: &str = "planner model selection";
/// The third argument of `require_rest_user_actor_for`, named for ITS
/// parameter (`redirect`). Two of that function's three parameters are
/// `&str`, so swapping them compiles and produces a 403 that names the wrong
/// subsystem — read the call site back against these names, not against the
/// build.
const ACTOR_REDIRECT: &str = "Which model a person's conversation runs with is their own choice; agents have no write \
     path to it.";

/// Accept `null`, refuse absence.
///
/// A field typed `Option<T>` is optional to serde: a missing key deserializes
/// to `None` with no complaint. Naming a `deserialize_with` takes that
/// special case away and the field becomes required, while still accepting an
/// explicit `null` as `None`. That is exactly the contract this body wants,
/// and it is pinned by `omitting_a_key_is_refused_and_changes_nothing`.
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
    /// The model **slug** to run with, or `null` to follow the installation
    /// default. Required — see the module header.
    ///
    /// A slug, never a catalog entry's `id`: `GET /api/models` returns both
    /// and only `model` is the one codex is invoked by.
    ///
    /// `#[schema(required = true)]` is not decoration and not a duplicate of
    /// [`required_nullable`]. The two live in different worlds: serde decides
    /// what the handler accepts, utoipa decides what the published
    /// OpenAPI document promises, and utoipa derives optionality from the `Option<T>` in the
    /// field type alone — it cannot see a `deserialize_with`. Without this the
    /// document said both fields were optional while the handler answered 422
    /// for omitting one, so a client generated from either checked-in copy was
    /// conforming and broken at the same time.
    #[schema(required = true)]
    #[serde(deserialize_with = "required_nullable")]
    pub model: Option<String>,
    /// The reasoning effort, or `null` to follow the default. Required.
    ///
    /// A bare string rather than a closed set: codex accepts any non-empty
    /// effort, so an enum here would start refusing values the day codex ships
    /// a new one.
    ///
    /// `#[schema(required = true)]` for the same reason as `model` above.
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
    /// The requested effort was not among the chosen model's supported ones
    /// and was moved to that model's own default. Never done silently — a
    /// caller that ignores this flag shows a value that will not run.
    pub effort_adjusted: bool,
    /// The slug is not in the catalog codex currently reports. A hint, not a
    /// refusal: the stored value is the requested one either way. Always
    /// `false` when the catalog could not be read, because "we could not ask"
    /// is not evidence of absence.
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
    Json(body): Json<SetPlannerModelBody>,
) -> Result<(StatusCode, Json<SetPlannerModelResponse>)> {
    // Ordering matches `planner_input::resolve`: the actor check runs first,
    // so an agent probing card ids learns nothing from the status it gets.
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

    let SetPlannerModelBody {
        model,
        reasoning_effort,
    } = body;
    let advice = catalog_advice(&codex, model.as_deref(), reasoning_effort.as_deref()).await;
    let stored_effort = advice
        .adjusted_to
        .clone()
        .or_else(|| reasoning_effort.clone());

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
                    // Read-modify-write inside the transaction. See the module
                    // header: the payload column is replaced wholesale.
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

    // A refusal the kernel could not resolve told the reader to pick a model.
    // They just did, so the harness must stop waiting out its 30 s interval
    // rather than making them watch their sentence sit there having done the
    // thing they were asked to do. Best-effort by construction: no live
    // harness means nothing was waiting.
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
        effort_adjusted = advice.adjusted_to.is_some(),
        unknown_model = advice.unknown_model,
        "planner conversation model selection stored"
    );

    Ok((
        StatusCode::OK,
        Json(SetPlannerModelResponse {
            card_id,
            model,
            reasoning_effort: stored_effort,
            effort_adjusted: advice.adjusted_to.is_some(),
            unknown_model: advice.unknown_model,
        }),
    ))
}

/// What the catalog says about a requested selection.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CatalogAdvice {
    /// `Some(effort)` when the requested effort is not supported by the chosen
    /// model and must land on that model's own default instead.
    pub(super) adjusted_to: Option<String>,
    unknown_model: bool,
}

/// Ask codex's catalog about the requested pair.
///
/// Every failure to ask yields [`CatalogAdvice::default`] — no adjustment, not
/// unknown. That is the fail-safe direction for a hint: claiming a model is
/// unknown because we could not reach a daemon would put a warning next to a
/// perfectly good choice, and moving somebody's effort on the strength of a
/// catalog we never read would be worse still. The stored selection is the
/// requested one in every one of these branches.
pub(super) async fn catalog_advice(
    codex: &CodexShellState,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> CatalogAdvice {
    // With no model chosen there is no catalog entry to judge against: the
    // effort is being applied to whatever the installation default resolves
    // to at turn time, which this handler does not get to see.
    let Some(model) = model else {
        return CatalogAdvice::default();
    };
    let deadline = tokio::time::Instant::now() + CODEX_READ_TIMEOUT;
    let models = match codex.shared_codex_appserver.model_list(deadline).await {
        Ok(models) => models,
        Err(e) => {
            tracing::warn!(error = %e, "planner model selection: model/list unavailable");
            return CatalogAdvice::default();
        }
    };
    let Some(entry) = models.into_iter().find(|m| m.model == model) else {
        return CatalogAdvice {
            adjusted_to: None,
            unknown_model: true,
        };
    };
    let Some(requested) = reasoning_effort else {
        return CatalogAdvice::default();
    };
    let supported = entry
        .supported_reasoning_efforts
        .iter()
        .any(|o| o.reasoning_effort == requested);
    CatalogAdvice {
        adjusted_to: (!supported).then(|| entry.default_reasoning_effort.clone()),
        unknown_model: false,
    }
}
