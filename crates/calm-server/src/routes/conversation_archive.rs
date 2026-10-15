//! Manual archive/restore, serialized with conversation sends and session starts.
use crate::actor::Actor;
use crate::db::{
    sqlite::{card_update_tx, conversation_archive_set_tx},
    write_with_event_typed,
};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::{JsonBody, Path};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::model::{CardPatch, CardRole};
use crate::operation::planner_start_fence::CardStartFence;
use crate::per_card_lock::lock_key;
use crate::state::{AppState, RouteState};
use axum::{Router, extract::State, http::StatusCode, routing::patch};
use serde::Deserialize;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/tracks/{track_id}/conversations/{card_id}/archive",
        patch(set_conversation_archive),
    )
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationArchiveRequest {
    pub archived: bool,
}

#[utoipa::path(
    patch, path = "/api/tracks/{track_id}/conversations/{card_id}/archive", tag = "tracks",
    params(("track_id" = String, Path, description = "Owning Track"), ("card_id" = String, Path, description = "Assistant conversation")),
    request_body = ConversationArchiveRequest,
    responses(
        (status = 204, description = "Archive state saved; repeating the same assignment changes nothing"),
        (status = 403, description = "Only the person can archive an Assistant conversation", body = ErrorBody),
        (status = 404, description = "Track or conversation not found", body = ErrorBody),
        (status = 409, description = "Reply or queued input still in progress", body = ErrorBody),
        (status = 500, description = "Archive state could not be saved", body = ErrorBody),
        (status = 503, description = "Conversation issuance is still settling; retry shortly", body = ErrorBody),
    )
)]
pub(crate) async fn set_conversation_archive(
    State(s): State<RouteState>,
    actor: Actor,
    Path((track_id, card_id)): Path<(String, String)>,
    JsonBody(body): JsonBody<ConversationArchiveRequest>,
) -> Result<StatusCode> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "conversation archive",
        "Only the person chooses which discussions are archived.",
    )?;
    let id = crate::ids::CardId::from(card_id.clone());
    let _card = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        &id,
    )
    .await;
    let _operations = s.operation_runtime.lock_for_track_delete().await;
    let _track = lock_key(&s.track_delete_locks, &track_id).await;
    let card = s
        .repo
        .card_get(&card_id)
        .await?
        .filter(|card| card.track_id.as_str() == track_id)
        .ok_or_else(|| {
            CalmError::NotFound(format!("conversation {card_id} on track {track_id}"))
        })?;
    let role = s.repo.card_role_get(&card_id).await?;
    let binding = role.and_then(|role| PlannerBinding::from_card(&card, role));
    if !binding.is_some_and(|binding| binding.profile == HarnessProfile::Assistant)
        || role != Some(CardRole::Assistant)
    {
        return Err(CalmError::Forbidden(
            "Only Assistant conversations can be archived".into(),
        ));
    }
    if s.repo.conversation_archived_at(&card_id).await?.is_some() == body.archived {
        return Ok(StatusCode::NO_CONTENT);
    }
    let runtime = if body.archived {
        s.repo
            .session_projection_projectable_for_card(&card_id)
            .await?
    } else {
        None
    };
    let handle = runtime
        .as_ref()
        .and_then(|runtime| s.harness.get(&runtime.id));
    // The turn owner holds this same fence from before its phase changes until
    // after queue drain/provider submission. Keep it through the archive commit.
    let _idle = if let Some(handle) = &handle {
        Some(handle.lock_if_idle().await?)
    } else {
        None
    };
    if let Some(runtime) = runtime.as_ref().filter(|_| handle.is_none()) {
        if let Some(snapshot) = runtime
            .handle_state_json
            .clone()
            .and_then(crate::harness::HarnessSnapshot::parse_known)
        {
            crate::conversation_archive::require_idle(&snapshot)?;
        } else if matches!(
            runtime.status,
            crate::session_projection_repo::WorkerSessionState::Starting
                | crate::session_projection_repo::WorkerSessionState::Running
                | crate::session_projection_repo::WorkerSessionState::TurnPending
        ) {
            return Err(CalmError::Conflict(
                "Conversation status is not confirmed; try again after it finishes.".into(),
            ));
        }
    }
    let track = s
        .repo
        .track_get(&track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let scope = EventScope::Card {
        card: card.id,
        track: track.id,
        area: track.area_id,
    };
    write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                conversation_archive_set_tx(tx, &card_id, body.archived).await?;
                let card = card_update_tx(tx, &card_id, CardPatch::default()).await?;
                Ok(((), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
