//! `PATCH` / `DELETE /api/cards/{id}/planner/input/{entry_id}` and `POST …/steer`:
//! rewrite, take back, or steer a queued planner message. DELETE is not idempotent
//! under rebuffer — a drained entry can be queued again — so a client must not retry it.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
use crate::harness::{HarnessPhaseTag, SteerRefused};
use crate::ids::{ActorId, CardId};
use crate::routes::cards::{card_runs_headless_harness, validate_planner_input_text};
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::{RouteState, WorkerState};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditPlannerInputBody {
    /// Replacement text, held to the same limits as `POST /planner/input`.
    pub text: String,
    /// The `rev` the client last read for this entry; a mismatch is a 409.
    pub if_entry_rev: u32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeletePlannerInputBody {
    /// The `rev` the client last read for this entry. Required.
    pub if_entry_rev: u32,
}

/// Body of `POST …/{entry_id}/steer`: the same compare-and-swap token as the delete.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerPlannerInputBody {
    pub if_entry_rev: u32,
}

/// What a steer answers on success: codex has the message inside `turn_id`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerSteerResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    pub entry_id: String,
    /// Always `true` on a 200; refusals are typed 409s, never `false` here.
    pub steered: bool,
    /// The turn that took the message — the one that was running.
    pub turn_id: String,
}

/// 409 body for a steer that delivered nothing (`planner_steer_no_running_turn`: the
/// message is still queued with the `rev` the client read) or nothing known
/// (`planner_steer_unknown_outcome`: codex did not answer, so the message is queued
/// again but may also have reached the turn).
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerSteerRefusedBody {
    pub error: String,
    /// `planner_steer_no_running_turn` or `planner_steer_unknown_outcome`.
    pub code: String,
    pub entry_id: String,
    pub phase: HarnessPhaseTag,
}

/// The steer route's 409: two typed shapes told apart by `code`. utoipa binds one
/// body per status, so the pair is declared as this untagged union.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum PlannerSteerConflictBody {
    Stale(PlannerInputStaleBody),
    Refused(PlannerSteerRefusedBody),
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerInputMutationResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    pub entry_id: String,
    /// The entry's `rev` after the change. On a delete this is the rev it
    /// carried when it left, so a client can tell which read it acted on.
    pub rev: u32,
    /// The entry's text after an edit; null for a delete.
    pub text: Option<String>,
}

/// 409 body for a compare-and-swap failure; carries the current text and rev.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerInputStaleBody {
    pub error: String,
    /// Always `planner_input_stale`.
    pub code: String,
    pub entry_id: String,
    /// The entry's text as it stands now.
    pub text: String,
    /// The entry's current rev — resend with this to overwrite deliberately.
    pub rev: u32,
}

const ACTOR_SUBJECT: &str = "planner input edit";
const ACTOR_REDIRECT: &str =
    "A queued message is the person's own un-sent intent; agents have no write path to it.";

/// Resolve the card, the live harness, and refuse anything that is not a human. The
/// actor check runs first, so an agent probing card ids learns nothing from the status.
async fn resolve(
    state: &RouteState,
    workers: &WorkerState,
    card_id: &str,
    actor: &Actor,
) -> Result<(CardId, String, crate::harness::PlannerHarness)> {
    require_rest_user_actor_for(actor, ACTOR_SUBJECT, ACTOR_REDIRECT)?;

    let card = state
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let role = state
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {card_id} is not a planner codex card",
        )));
    }

    // No lazy restart here, unlike `POST /planner/input`: a harness that is not running
    // holds no queue, so the answer is the same 404 a drained entry gets.
    let absent = || {
        CalmError::NotFound(format!(
            "planner input entry for card {card_id}: no live planner harness session holds a \
             pending queue",
        ))
    };
    let runtime = state
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(absent)?;
    let harness = workers.harness.get(&runtime.id).ok_or_else(absent)?;
    Ok((card.id, runtime.id, harness))
}

fn refusal_response(card_id: &CardId, refused: MutationRefused) -> Response {
    match refused {
        MutationRefused::NotFound => CalmError::NotFound(format!(
            "planner input entry for card {card_id}: it is no longer in the pending queue",
        ))
        .into_response(),
        MutationRefused::Stale {
            entry_id,
            text,
            rev,
        } => (
            StatusCode::CONFLICT,
            Json(PlannerInputStaleBody {
                error: format!(
                    "planner input entry {entry_id} changed since you read it; it is now at rev \
                     {rev}",
                ),
                code: "planner_input_stale".into(),
                entry_id: entry_id.as_str().to_string(),
                text,
                rev,
            }),
        )
            .into_response(),
        MutationRefused::AmbiguousId { entry_id, count } => {
            tracing::error!(
                card_id = %card_id,
                entry_id = %entry_id,
                count,
                "planner pending queue holds more than one entry with the same id"
            );
            CalmError::Conflict(format!(
                "planner input entry {entry_id} is not uniquely identified in the pending queue; \
                 refusing to guess which one you meant",
            ))
            .into_response()
        }
    }
}

async fn mutate(
    state: RouteState,
    workers: WorkerState,
    card_id: String,
    entry_id: String,
    actor: Actor,
    build: impl FnOnce(QueueEntryId) -> QueueMutation,
) -> Result<Response> {
    let (card_id, worker_session_id, harness) = resolve(&state, &workers, &card_id, &actor).await?;
    let mutation = build(QueueEntryId::from_wire(entry_id));
    match harness
        .mutate_pending_entry(mutation, ActorId::User)
        .await?
    {
        Ok(applied) => {
            tracing::info!(
                actor = %actor.as_str(),
                card_id = %card_id,
                worker_session_id = %worker_session_id,
                entry_id = %applied.entry_id,
                change = ?applied.change,
                "planner harness pending queue entry mutated"
            );
            Ok(Json(PlannerInputMutationResponse {
                card_id,
                worker_session_id,
                entry_id: applied.entry_id.as_str().to_string(),
                rev: applied.rev,
                text: applied.text,
            })
            .into_response())
        }
        Err(refused) => Ok(refusal_response(&card_id, refused)),
    }
}

#[utoipa::path(
    patch,
    path = "/api/cards/{id}/planner/input/{entry_id}",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("entry_id" = String, Path, description = "Pending queue entry id from `GET /planner/run`"),
    ),
    request_body = EditPlannerInputBody,
    responses(
        (status = 200, description = "Entry rewritten", body = PlannerInputMutationResponse),
        (status = 400, description = "Empty or over-long text", body = ErrorBody),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found, or the entry is no longer in the pending queue", body = ErrorBody),
        (status = 409, description = "`if_entry_rev` does not match (code `planner_input_stale`, body carries the current text and rev), or the harness is shutting down (code `conflict`)", body = PlannerInputStaleBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Harness command channel saturated — retry shortly", body = ErrorBody),
    ),
)]
pub(crate) async fn edit_planner_input(
    State(state): State<RouteState>,
    State(workers): State<WorkerState>,
    _principal: Principal,
    actor: Actor,
    Path((id, entry_id)): Path<(String, String)>,
    Json(body): Json<EditPlannerInputBody>,
) -> Result<Response> {
    validate_planner_input_text(&body.text)?;
    let EditPlannerInputBody { text, if_entry_rev } = body;
    mutate(state, workers, id, entry_id, actor, move |entry_id| {
        QueueMutation::Edit {
            entry_id,
            text,
            if_entry_rev,
        }
    })
    .await
}

#[utoipa::path(
    delete,
    path = "/api/cards/{id}/planner/input/{entry_id}",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("entry_id" = String, Path, description = "Pending queue entry id from `GET /planner/run`"),
    ),
    request_body = DeletePlannerInputBody,
    responses(
        (status = 200, description = "Entry removed from the queue before it was delivered", body = PlannerInputMutationResponse),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found, or the entry is no longer in the pending queue — it may already have been delivered. Not retryable: a re-issued DELETE can answer 200 if the batch was rebuffered.", body = ErrorBody),
        (status = 409, description = "`if_entry_rev` does not match (code `planner_input_stale`), or the harness is shutting down (code `conflict`)", body = PlannerInputStaleBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Harness command channel saturated — retry shortly", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_planner_input(
    State(state): State<RouteState>,
    State(workers): State<WorkerState>,
    _principal: Principal,
    actor: Actor,
    Path((id, entry_id)): Path<(String, String)>,
    Json(body): Json<DeletePlannerInputBody>,
) -> Result<Response> {
    let if_entry_rev = body.if_entry_rev;
    mutate(state, workers, id, entry_id, actor, move |entry_id| {
        QueueMutation::Delete {
            entry_id,
            if_entry_rev,
        }
    })
    .await
}

/// Kept beside PATCH/DELETE rather than behind the operation adapter, which flattens
/// every failure to a class and a message and would lose the two typed 409s.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/input/{entry_id}/steer",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("entry_id" = String, Path, description = "Pending queue entry id from `GET /planner/run`"),
    ),
    request_body = SteerPlannerInputBody,
    responses(
        (status = 200, description = "The entry left the queue and codex took it into the running turn", body = PlannerSteerResponse),
        (status = 401, description = "Unauthenticated", body = ErrorBody),
        (status = 403, description = "Not `X-Calm-Actor: user`, or the card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found, or the entry is no longer in the pending queue", body = ErrorBody),
        (status = 409, description = "By `code`: `planner_input_stale`; `planner_steer_no_running_turn`; \
                                      `planner_steer_unknown_outcome`; `conflict` (shutting down)", body = PlannerSteerConflictBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Harness command channel saturated — retry shortly", body = ErrorBody),
    ),
)]
pub(crate) async fn steer_planner_input(
    State(state): State<RouteState>,
    State(workers): State<WorkerState>,
    _principal: Principal,
    actor: Actor,
    Path((id, entry_id)): Path<(String, String)>,
    Json(body): Json<SteerPlannerInputBody>,
) -> Result<Response> {
    let (card_id, worker_session_id, harness) = resolve(&state, &workers, &id, &actor).await?;
    match harness
        .steer_pending_entry(
            QueueEntryId::from_wire(entry_id.clone()),
            body.if_entry_rev,
            ActorId::User,
        )
        .await?
    {
        Ok(applied) => {
            tracing::info!(
                actor = %actor.as_str(),
                card_id = %card_id,
                worker_session_id = %worker_session_id,
                entry_id = %applied.entry_id,
                turn_id = %applied.turn_id,
                "planner harness pending queue entry steered into the running turn"
            );
            Ok(Json(PlannerSteerResponse {
                card_id,
                worker_session_id,
                entry_id: applied.entry_id.as_str().to_string(),
                steered: true,
                turn_id: applied.turn_id,
            })
            .into_response())
        }
        Err(SteerRefused::Queue(refused)) => Ok(refusal_response(&card_id, refused)),
        Err(SteerRefused::NoRunningTurn { phase }) => Ok(steer_refused_response(
            entry_id,
            phase,
            "planner_steer_no_running_turn",
            format!(
                "no turn is running right now (phase `{}`), so there is nothing to steer; the \
                 message is still queued and will go with the next turn",
                phase_wire_name(phase)
            ),
        )),
        Err(SteerRefused::NotTaken { message, phase }) => Ok(steer_refused_response(
            entry_id,
            phase,
            "planner_steer_no_running_turn",
            format!(
                "codex did not take the message into the running turn ({message}); it is still \
                 queued and will go with the next turn"
            ),
        )),
        Err(SteerRefused::Unanswered { message, phase }) => Ok(steer_refused_response(
            entry_id,
            phase,
            "planner_steer_unknown_outcome",
            format!(
                "codex did not answer in time ({message}), so it is not known whether the message \
                 reached the running turn; it is queued again and will go with the next turn"
            ),
        )),
    }
}

/// The wire spelling of a phase (`turn_running`, not `TurnRunning`).
fn phase_wire_name(phase: HarnessPhaseTag) -> String {
    serde_json::to_value(phase)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn steer_refused_response(
    entry_id: String,
    phase: HarnessPhaseTag,
    code: &str,
    error: String,
) -> Response {
    (
        StatusCode::CONFLICT,
        Json(PlannerSteerConflictBody::Refused(PlannerSteerRefusedBody {
            error,
            code: code.into(),
            entry_id,
            phase,
        })),
    )
        .into_response()
}
