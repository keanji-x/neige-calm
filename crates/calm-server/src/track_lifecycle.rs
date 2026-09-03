//! Issue #145 — Track lifecycle state machine, transaction-side helpers.
//!
//! #679 PR1: the pure edge table — [`ActorKind`], [`actor_kind`],
//! [`validate_transition`], [`TransitionError`] — moved to
//! `calm_types::track_lifecycle` (zero-IO vocabulary; PR0's
//! `track_fsm_golden` pins the table itself) and is re-exported below so
//! every `crate::track_lifecycle::validate_transition` path is unchanged.
//! This file keeps the sqlx-transaction helpers that apply validated
//! transitions inside audited write transactions.

use crate::db::rows::TRACK_SELECT_COLUMNS;
use crate::model::{Track, TrackLifecycle, TrackPatch};
use crate::{error::CalmError, event::Event};
use sqlx::{Sqlite, Transaction};

// #679 PR1 — moved vocabulary, re-exported at the old paths. Source
// definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::track_lifecycle::{
    ActorKind, TransitionError, actor_is_planner_author, actor_kind, validate_transition,
};

/// Auto-promote a draft track to planning from inside an audited write tx.
///
/// Returns the lifecycle/update events the caller should append to the same
/// event batch. Non-draft tracks are left untouched and return `None`.
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

/// Apply an explicit planner-requested lifecycle transition inside the caller's
/// write tx and return the lifecycle/update events for the same batch.
///
/// If the requested target equals current lifecycle, no lifecycle events are
/// emitted and the caller's `agent_message` is discarded. This is intentional —
/// without a transition there is no lifecycle event to carry the message, and
/// bumping `TrackUpdated.agent_message` on a no-op would emit a spurious event.
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

/// Auto-transition a track when it is exactly in `from`.
///
/// Kernel auto hooks use this for idempotent current-state gating: only the
/// first serialized tx sees the triggering `from` state, updates the row, and
/// emits lifecycle/update events; later concurrent txs see the advanced state
/// and do nothing.
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

/// In-tx track row read. `pub(crate)` since #955 PR-b: the proposal
/// submit handler re-checks "track exists and is not terminal" inside its
/// own write transaction (§5.5), which an outside-tx read cannot make
/// authoritative.
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
