//! #1505 PR2 — `PATCH` / `DELETE /api/cards/{id}/planner/input/{entry_id}`,
//! and since #1625 P3 `POST …/{entry_id}/steer`.
//!
//! The queue a planner card accumulates while a turn runs used to be
//! write-once: a message could be sent into it and then only waited out. These
//! routes make an entry a thing the person who wrote it can still change their
//! mind about — rewrite it, take it back, or (P3) send it into the turn that
//! is running right now instead of waiting for the next one.
//!
//! Three decisions are worth reading before the code.
//!
//! **Addressing is by id, never by index or by text.** A position shifts the
//! moment the queue drains, and two identical sends are indistinguishable by
//! body — either would mean "delete" could land on somebody else's sentence.
//! The id comes from #1505 PR1 and is the same one `GET /planner/run` shows.
//!
//! **Both routes take `if_entry_rev`, and it is required on the delete too.**
//! "I am deleting what I read" is the same precondition as "I am editing what
//! I read"; an optional token is an unconditional write for whoever leaves it
//! out, and two tabs open on one card is not an exotic setup.
//!
//! **A drained entry answers 404, not "already sent".** A 404 here means "it is
//! not in the queue, re-read", which is true in every case that produces it. A
//! "already sent" would be a guess: `rebuffer_head` puts a failed batch back at
//! the head, so an entry that has drained can be queued again, and neither a
//! restart nor a snapshot truncation leaves anything behind to tell the two
//! apart. This also makes DELETE non-idempotent under rebuffer — a client must
//! not retry it.

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
    /// Replacement text. Held to the same limits as `POST /planner/input`,
    /// because it becomes the same turn input.
    pub text: String,
    /// The `rev` the client last read for this entry. A mismatch is a 409, not
    /// a silent overwrite.
    pub if_entry_rev: u32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeletePlannerInputBody {
    /// The `rev` the client last read for this entry. Required — see the
    /// module header.
    pub if_entry_rev: u32,
}

/// #1625 P3 — body of `POST …/{entry_id}/steer`. The same compare-and-swap
/// token as the delete, for the same reason: "send the message I read" is a
/// precondition on the text, and a steer that ignored it could deliver a
/// sentence somebody else had just rewritten.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerPlannerInputBody {
    pub if_entry_rev: u32,
}

/// #1625 P3 — what a steer answers on success: codex has the message inside
/// `turn_id`, and the entry is no longer in the queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerSteerResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    pub entry_id: String,
    /// Always `true` on a 200, in the shape of `POST /planner/interrupt`'s
    /// `stopped`: the refusals are typed 409s, never a `false` here.
    pub steered: bool,
    /// The turn that took the message — the one that was running.
    pub turn_id: String,
}

/// #1625 P3 — 409 body for a steer that delivered nothing, or nothing known.
///
/// Two codes. `planner_steer_no_running_turn` covers both ways of KNOWING
/// nothing was delivered — the harness saw no running turn and did not ask,
/// or codex was asked and said no (the turn had just ended, or a different
/// one was running) — because they license the same next move and nothing
/// else: the message is still queued, with the `rev` the client read, and it
/// goes with the next turn. `planner_steer_unknown_outcome` (review round 1)
/// is codex NOT answering — the request timed out or the connection dropped
/// — where the message is queued again just the same but may ALSO have
/// reached the turn; it is its own code because "nothing happened" would be
/// a claim this side cannot make. `error` says which, for the person reading
/// the notice; `phase` is the harness's own phase at the moment it answered.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerSteerRefusedBody {
    pub error: String,
    /// `planner_steer_no_running_turn` or `planner_steer_unknown_outcome`.
    pub code: String,
    pub entry_id: String,
    pub phase: HarnessPhaseTag,
}

/// #1625 P3 review round 1 — the steer route's 409, which has two typed
/// shapes told apart by `code`: `PlannerInputStaleBody` (`planner_input_stale`,
/// the compare-and-swap lost; carries the entry's current text and rev) and
/// `PlannerSteerRefusedBody` (`planner_steer_no_running_turn` /
/// `planner_steer_unknown_outcome`, the entry is still queued). utoipa binds
/// one body per status, so the pair is declared as this untagged union; the
/// stale arm is produced by `refusal_response`, shared with PATCH/DELETE, and
/// the refused arm by `steer_refused_response`. A third 409, the harness
/// shutting down, is a plain `ErrorBody` with code `conflict`.
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
    ///
    /// Echoed rather than assumed: the client is then reconciling against what
    /// the queue holds rather than against what it hoped it would hold. The
    /// queue as a whole is deliberately NOT returned — this same mutation emits
    /// `harness.queue.changed`, which invalidates the planner-run query, so a
    /// list attached here would be superseded before it could be used.
    pub text: Option<String>,
}

/// 409 body for a compare-and-swap failure.
///
/// It carries the current text and rev because the alternative is a client
/// that has to issue a read to find out what it collided with, and the read it
/// would issue can be superseded again before it lands. The extra fields sit
/// alongside `error`/`code` so a generic error handler still sees the shape it
/// expects.
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
/// The third argument of `require_rest_user_actor_for`, named for ITS
/// parameter (`redirect`) rather than for this module's older `hint`: two of
/// that function's three parameters are `&str`, so a swap compiles, and a name
/// that does not match the slot it fills is how the swap gets made.
const ACTOR_REDIRECT: &str =
    "A queued message is the person's own un-sent intent; agents have no write path to it.";

/// Resolve the card, the live harness, and refuse anything that is not a human.
///
/// Ordering is deliberate: the actor check runs first, so an agent probing card
/// ids learns nothing from the status it gets back.
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

    // No lazy restart here, unlike `POST /planner/input`. Spawning a runtime
    // in order to edit its queue is backwards: a harness that is not running
    // holds no queue, so the only honest answer is the same 404 a drained entry
    // gets — the entry is not in the queue.
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
            // Refused rather than resolved: see `MutationRefused::AmbiguousId`.
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
    // `ActorId::User` is not a shortcut past `actor`: `require_rest_user_actor_for`
    // has already refused everything else, so this is the actor, spelled in the
    // vocabulary the event carries.
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

/// #1625 P3 — the third verb on the entry, kept beside PATCH/DELETE rather than
/// behind the operation adapter for the reason the module header gives for
/// those two: `run_planner_card_operation` flattens every failure to a class
/// and a message, and the two typed 409s this route answers with would not
/// survive it.
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
        // Four codes share this status; `PlannerSteerConflictBody` names the
        // two typed shapes, and `PlannerSteerRefusedBody` the steer's own
        // codes (the entry stays queued and drains into the next turn).
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

/// The wire spelling of a phase (`turn_running`, not `TurnRunning`), so the
/// sentence a person reads matches what `GET /planner/run` shows them.
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
