//! `POST /api/tracks/{id}/activity/dismissals` (#1829): the user sets one notification item of the
//! track's activity overlay aside. The key is stored, not the item: the projector drops a stored key
//! from its track's items, and the same source happening again has a new key, so it lights up again.

use axum::{Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::actor::Actor;
use crate::db::sqlite::track_get_tx;
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{JsonBody, Path};
use crate::ids::TrackId;
use crate::model::now_ms;
use crate::state::{AppState, RouteState};
use crate::track_activity::notifications::is_item_key;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/tracks/{id}/activity/dismissals",
        post(dismiss_activity_item),
    )
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DismissActivityItemRequest {
    /// The item's `key` as the activity overlay lists it: `ask:ratify:<id>`, `ask:notify:<id>`
    /// or `planner_down:<id>`. Whether the item is still open is not checked.
    pub key: String,
}

/// Dismiss one notification item. Idempotent: a key already dismissed keeps its first time. No event
/// is written; the projector is woken in-process and its `overlay.set` is what the client sees.
#[utoipa::path(
    post,
    path = "/api/tracks/{id}/activity/dismissals",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = DismissActivityItemRequest,
    responses(
        (status = 204, description = "Dismissed (or already dismissed)"),
        (status = 400, description = "The key is not an item key", body = ErrorBody),
        (status = 403, description = "The actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn dismiss_activity_item(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<DismissActivityItemRequest>,
) -> Result<StatusCode> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "activity dismissal",
        "Only the user dismisses a notification.",
    )?;
    if !is_item_key(&body.key) {
        return Err(CalmError::BadRequest(format!(
            "activity dismissal: `{}` is not an item key \
             (ask:ratify:<id>, ask:notify:<id> or planner_down:<id>)",
            body.key
        )));
    }
    let track = TrackId::from(id.clone());
    let key = body.key;
    write_in_tx_typed(s.repo.as_ref(), move |tx| {
        Box::pin(async move {
            track_get_tx(tx, &track).await?;
            sqlx::query(
                "INSERT INTO activity_dismissals (track_id, item_key, dismissed_at_ms) \
                 VALUES (?1, ?2, ?3) ON CONFLICT(track_id, item_key) DO NOTHING",
            )
            .bind(track.as_str())
            .bind(&key)
            .bind(now_ms())
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await?;
    s.activity_wake.wake(&id);
    Ok(StatusCode::NO_CONTENT)
}
