//! Track lifecycle transaction-side helpers; the pure edge table lives in `calm_types::track_lifecycle`.

use crate::db::rows::TRACK_SELECT_COLUMNS;
use crate::model::{Track, TrackLifecycle, TrackPatch};
use crate::{error::CalmError, event::Event};
use sqlx::{Sqlite, Transaction};

// Source definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::track_lifecycle::{
    ActorKind, TransitionError, actor_is_planner_author, actor_kind, planner_allowed_targets,
    user_can_resume, validate_transition,
};

/// Auto-promote a draft track to planning from inside an audited write tx; non-draft tracks return `None`.
pub async fn auto_promote_draft_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &crate::ids::TrackId,
) -> Result<Option<Vec<Event>>, CalmError> {
    auto_transition_if_current_in_tx(
        tx,
        track_id,
        TrackLifecycle::Draft,
        TrackLifecycle::Planning,
        &crate::ids::ActorId::Kernel,
        Some("[auto] first planner write".to_string()),
    )
    .await
}

/// Apply a planner-requested lifecycle transition inside the caller's write tx. A no-op target
/// discards `agent_message` on purpose: without a transition there is no lifecycle event to carry it.
pub async fn apply_requested_transition_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &crate::ids::TrackId,
    to: TrackLifecycle,
    actor: &crate::ids::ActorId,
    agent_message: String,
) -> Result<Option<Vec<Event>>, CalmError> {
    let current = track_get_tx(tx, track_id).await?;
    validate_transition(current.lifecycle, to, actor)
        .map_err(|e| CalmError::Forbidden(format!("track lifecycle: {e}")))?;
    if current.lifecycle == to {
        return Ok(None);
    }
    let updated = crate::db::sqlite::track_update_tx(
        tx,
        track_id.as_str(),
        TrackPatch {
            lifecycle: Some(to),
            ..TrackPatch::default()
        },
    )
    .await?;
    Ok(Some(vec![
        Event::TrackLifecycleChanged {
            id: updated.id.clone(),
            area_id: updated.area_id.clone(),
            from: current.lifecycle,
            to,
            agent_message: Some(agent_message.clone()),
        },
        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
            updated,
            Some(agent_message),
        )),
    ]))
}

/// Re-check a REST handler's pre-transaction lifecycle snapshot inside the IMMEDIATE tx: a stale
/// snapshot is a retryable conflict, since writing from it would persist an edge the FSM never approved.
pub async fn validate_transition_snapshot_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &crate::ids::TrackId,
    expected_from: TrackLifecycle,
    to: TrackLifecycle,
    actor: &crate::ids::ActorId,
) -> Result<(), CalmError> {
    let current = track_get_tx(tx, track_id).await?;
    if current.lifecycle != expected_from {
        return Err(CalmError::Conflict(format!(
            "track {} lifecycle changed from {expected_from:?} to {:?}; retry",
            track_id, current.lifecycle
        )));
    }
    validate_transition(current.lifecycle, to, actor)
        .map_err(|e| CalmError::Forbidden(format!("track lifecycle: {e}")))
}

/// Auto-transition a track when it is exactly in `from`: only the first serialized tx sees the
/// triggering state; later concurrent txs see the advanced state and do nothing.
pub async fn auto_transition_if_current_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &crate::ids::TrackId,
    from: TrackLifecycle,
    to: TrackLifecycle,
    actor: &crate::ids::ActorId,
    agent_message: Option<String>,
) -> Result<Option<Vec<Event>>, CalmError> {
    let current = track_get_tx(tx, track_id).await?;
    if current.lifecycle != from {
        return Ok(None);
    }
    validate_transition(current.lifecycle, to, actor)
        .map_err(|e| CalmError::Forbidden(format!("track lifecycle: {e}")))?;
    if current.lifecycle == to {
        return Ok(None);
    }
    let updated = crate::db::sqlite::track_update_tx(
        tx,
        track_id.as_str(),
        TrackPatch {
            lifecycle: Some(to),
            ..TrackPatch::default()
        },
    )
    .await?;
    Ok(Some(vec![
        Event::TrackLifecycleChanged {
            id: updated.id.clone(),
            area_id: updated.area_id.clone(),
            from: current.lifecycle,
            to,
            agent_message: agent_message.clone(),
        },
        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
            updated,
            agent_message,
        )),
    ]))
}

/// In-tx track row read, for handlers that must re-check the track inside their own write transaction.
pub(crate) async fn track_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &crate::ids::TrackId,
) -> Result<Track, CalmError> {
    sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE id = ?1"
    ))
    .bind(track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .map(Track::from)
    .ok_or_else(|| CalmError::NotFound(format!("track {}", track_id.as_str())))
}
