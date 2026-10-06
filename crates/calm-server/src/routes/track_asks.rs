//! `POST /api/tracks/{id}/asks/{ask_id}/answer` (#2209): the user answers one `neige_user_ask`.
//! The answer is `ask.answered`, the only record that a question was answered; it wakes the Planner.

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::db::write_with_actor_events_typed;
use crate::error::{ErrorBody, Result};
use crate::ids::{ActorId, TrackId};
use crate::json_body::JsonBody;
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tracks/{id}/asks/{ask_id}/answer", post(answer_ask))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerAskRequest {
    /// One answer per question of the ask, in its order. An answer need not be one of the options.
    pub answers: Vec<String>,
}

/// Answer every question of one ask. The ask must belong to this track and be unanswered; the
/// check and the write share one immediate transaction.
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
        (status = 400, description = "Wrong number of answers, or an empty or over-long one", body = ErrorBody),
        (status = 403, description = "The actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "No such ask on this track, or no such track", body = ErrorBody),
        (status = 409, description = "The ask is already answered", body = ErrorBody),
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
    let answers = body.answers;
    write_with_actor_events_typed::<(), _>(s.repo.as_ref(), None, &s.events, &s.write, {
        move |tx| {
            Box::pin(async move {
                let (scope, event) =
                    crate::ask::ask_answered_tx(tx, &track, ask_id, answers).await?;
                Ok(((), vec![(ActorId::User, scope, event)]))
            })
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
