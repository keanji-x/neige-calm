//! Issue #145 — Wave lifecycle state machine, transaction-side helpers.
//!
//! #679 PR1: the pure edge table — [`ActorKind`], [`actor_kind`],
//! [`validate_transition`], [`TransitionError`] — moved to
//! `calm_types::wave_lifecycle` (zero-IO vocabulary; PR0's
//! `wave_fsm_golden` pins the table itself) and is re-exported below so
//! every `crate::wave_lifecycle::validate_transition` path is unchanged.
//! This file keeps the sqlx-transaction helpers that apply validated
//! transitions inside audited write transactions.

use crate::model::{Wave, WaveLifecycle, WavePatch};
use crate::{error::CalmError, event::Event};
use sqlx::{Sqlite, Transaction};

// #679 PR1 — moved vocabulary, re-exported at the old paths. Source
// definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::wave_lifecycle::{
    ActorKind, TransitionError, actor_is_spec_author, actor_kind, validate_transition,
};

/// Auto-promote a draft wave to planning from inside an audited write tx.
///
/// Returns the lifecycle/update events the caller should append to the same
/// event batch. Non-draft waves are left untouched and return `None`.
pub async fn auto_promote_draft_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &crate::ids::WaveId,
) -> Result<Option<Vec<Event>>, CalmError> {
    auto_transition_if_current_in_tx(
        tx,
        wave_id,
        WaveLifecycle::Draft,
        WaveLifecycle::Planning,
        &crate::ids::ActorId::Kernel,
        Some("[auto] first spec write".to_string()),
    )
    .await
}

/// Apply an explicit spec-requested lifecycle transition inside the caller's
/// write tx and return the lifecycle/update events for the same batch.
///
/// If the requested target equals current lifecycle, no lifecycle events are
/// emitted and the caller's `agent_message` is discarded. This is intentional —
/// without a transition there is no lifecycle event to carry the message, and
/// bumping `WaveUpdated.agent_message` on a no-op would emit a spurious event.
pub async fn apply_requested_transition_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &crate::ids::WaveId,
    to: WaveLifecycle,
    actor: &crate::ids::ActorId,
    agent_message: String,
) -> Result<Option<Vec<Event>>, CalmError> {
    let current = wave_get_tx(tx, wave_id).await?;
    validate_transition(current.lifecycle, to, actor)
        .map_err(|e| CalmError::Forbidden(format!("wave lifecycle: {e}")))?;
    if current.lifecycle == to {
        return Ok(None);
    }
    let updated = crate::db::sqlite::wave_update_tx(
        tx,
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(to),
            ..WavePatch::default()
        },
    )
    .await?;
    Ok(Some(vec![
        Event::WaveLifecycleChanged {
            id: updated.id.clone(),
            cove_id: updated.cove_id.clone(),
            from: current.lifecycle,
            to,
            agent_message: Some(agent_message.clone()),
        },
        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
            updated,
            Some(agent_message),
        )),
    ]))
}

/// Auto-transition a wave when it is exactly in `from`.
///
/// Kernel auto hooks use this for idempotent current-state gating: only the
/// first serialized tx sees the triggering `from` state, updates the row, and
/// emits lifecycle/update events; later concurrent txs see the advanced state
/// and do nothing.
pub async fn auto_transition_if_current_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &crate::ids::WaveId,
    from: WaveLifecycle,
    to: WaveLifecycle,
    actor: &crate::ids::ActorId,
    agent_message: Option<String>,
) -> Result<Option<Vec<Event>>, CalmError> {
    let current = wave_get_tx(tx, wave_id).await?;
    if current.lifecycle != from {
        return Ok(None);
    }
    validate_transition(current.lifecycle, to, actor)
        .map_err(|e| CalmError::Forbidden(format!("wave lifecycle: {e}")))?;
    if current.lifecycle == to {
        return Ok(None);
    }
    let updated = crate::db::sqlite::wave_update_tx(
        tx,
        wave_id.as_str(),
        WavePatch {
            lifecycle: Some(to),
            ..WavePatch::default()
        },
    )
    .await?;
    Ok(Some(vec![
        Event::WaveLifecycleChanged {
            id: updated.id.clone(),
            cove_id: updated.cove_id.clone(),
            from: current.lifecycle,
            to,
            agent_message: agent_message.clone(),
        },
        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
            updated,
            agent_message,
        )),
    ]))
}

async fn wave_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &crate::ids::WaveId,
) -> Result<Wave, CalmError> {
    sqlx::query_as::<_, crate::db::rows::WaveRow>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, terminal_at, created_at, updated_at
           FROM waves WHERE id = ?1"#,
    )
    .bind(wave_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from)
    .ok_or_else(|| CalmError::NotFound(format!("wave {}", wave_id.as_str())))
}
