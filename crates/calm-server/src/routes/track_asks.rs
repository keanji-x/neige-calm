//! `POST /api/tracks/{id}/asks/{ask_id}/answer` (#2209, #2348): the user answers one ask. The
//! answer is `ask.answered`, the only record that a question was answered. A `wake` ask's answer
//! wakes the Planner; a `hold` ask's answer goes to the provider request its Planner's running
//! turn is paused on, through the harness's held-request table.

use axum::{Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{AskAnswer, AskDelivery};
use crate::extract::{JsonBody, Path};
use crate::ids::{ActorId, TrackId};
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tracks/{id}/asks/{ask_id}/answer", post(answer_ask))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerAskRequest {
    /// One answer per question of the ask, in its order: `{"option": i}` for the question's
    /// `i`-th option, or `{"text": ..}` for typed text. A paused request takes options only.
    pub answers: Vec<AskAnswer>,
}

/// Answer every question of one ask. The ask must belong to this track and still be open; the
/// check and the write share one immediate transaction. The work runs on its own task, so a
/// cancelled request never stores an answer without handing it to the paused provider request.
#[utoipa::path(
    post,
    path = "/api/tracks/{id}/asks/{ask_id}/answer",
    tag = "tracks",
    params(
        ("id" = String, Path, description = "Track id"),
        ("ask_id" = i64, Path, description = "The `ask.requested` event id"),
    ),
    request_body = AnswerAskRequest,
    responses(
        (status = 204, description = "Answered"),
        (status = 400, description = "Wrong number of answers, an option the question does not have, an empty or over-long text, or text for a paused request", body = ErrorBody),
        (status = 403, description = "The actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "No such ask on this track, or no such track", body = ErrorBody),
        (status = 409, description = "The ask is no longer open: it is answered, or its paused request is gone", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn answer_ask(
    State(s): State<RouteState>,
    actor: Actor,
    Path((id, ask_id)): Path<(String, i64)>,
    JsonBody(body): JsonBody<AnswerAskRequest>,
) -> Result<StatusCode> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "ask answer",
        "Only the user answers a question.",
    )?;
    let track = TrackId::from(id);
    tokio::spawn(answer(s, track, ask_id, body.answers))
        .await
        .map_err(|error| CalmError::Internal(format!("ask answer task failed: {error}")))??;
    Ok(StatusCode::NO_CONTENT)
}

/// Store the answer and, for a `hold` ask, hand it to the paused request. The harness's table
/// must hold the request before anything is stored (409 otherwise); after the commit the entry is
/// taken out of the table and answered at once. Whoever takes an entry answers it: when the
/// request went away between the two steps, the stored answer has no effect.
async fn answer(s: RouteState, track: TrackId, ask_id: i64, answers: Vec<AskAnswer>) -> Result<()> {
    let ask = crate::ask::stored_ask(s.repo.as_ref(), &track, ask_id).await?;
    let answers = crate::ask::validate_answers(ask_id, &ask.questions, ask.delivery, answers)?;
    let holder = match ask.delivery {
        AskDelivery::Wake => None,
        AskDelivery::Hold => {
            // A hold ask has one question, answered by one option (`validate_answers`).
            let [AskAnswer::Option(option)] = answers.as_slice() else {
                return Err(CalmError::BadRequest(format!(
                    "ask {ask_id} waits on a paused request; answer its one question with an option"
                )));
            };
            let harness = ask
                .holding_session()
                .and_then(|session| s.harness.get(&session.as_str().to_string()))
                .filter(|harness| harness.held_requests().contains(ask_id))
                .ok_or_else(|| {
                    CalmError::Conflict(format!(
                        "ask {ask_id} is no longer open: its paused request is gone"
                    ))
                })?;
            Some((harness, *option))
        }
    };
    write_with_actor_events_typed::<(), _>(s.repo.as_ref(), None, &s.events, &s.write, {
        let track = track.clone();
        move |tx| {
            Box::pin(async move {
                let (scope, event) =
                    crate::ask::ask_answered_tx(tx, &track, ask_id, answers).await?;
                Ok(((), vec![(ActorId::User, scope, event)]))
            })
        }
    })
    .await?;
    if let Some((harness, option)) = holder {
        match harness.held_requests().take(ask_id) {
            Some(responder) => responder.respond(option),
            None => tracing::info!(
                %track,
                ask_id,
                "the paused request went away after the answer was stored; it has no effect"
            ),
        }
    }
    Ok(())
}
