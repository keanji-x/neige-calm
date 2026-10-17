//! The existing worker-session snapshot is the sole pending authority. Every
//! canonical mutation loads and fences its exact current carrier in the caller's
//! transaction, then publishes only the committed snapshot to runtime caches.
use crate::error::{CalmError, Result};
use calm_types::conversation::{CONVERSATION_INPUT_CHECKPOINT_KEY, ConversationInputCheckpoint};
use calm_types::harness::{HARNESS_MODE, HarnessPhaseTag};
use serde_json::Value;
use sqlx::{Sqlite, Transaction};

pub struct CurrentInputCarrier {
    pub(super) card_id: String,
    pub(super) worker_session_id: String,
    pub(super) checkpoint: ConversationInputCheckpoint,
    pub(super) snapshot: Value,
    original_snapshot: String,
}

impl CurrentInputCarrier {
    pub fn checkpoint(&self) -> &ConversationInputCheckpoint {
        &self.checkpoint
    }
    pub fn snapshot(&self) -> &Value {
        &self.snapshot
    }
    pub fn worker_session_id(&self) -> &str {
        &self.worker_session_id
    }
    pub fn card_id(&self) -> &str {
        &self.card_id
    }

    pub(super) fn phase(&self) -> Result<HarnessPhaseTag> {
        let value = self
            .snapshot
            .get("phase")
            .ok_or_else(|| CalmError::Conflict("canonical carrier has no phase"))?;
        Ok(serde_json::from_value(value.clone())?)
    }

    pub(super) fn set_phase(&mut self, phase: HarnessPhaseTag) -> Result<()> {
        self.snapshot["phase"] = serde_json::to_value(phase)?;
        Ok(())
    }
}

pub async fn current_input_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<CurrentInputCarrier> {
    let original_snapshot: Option<String> = sqlx::query_scalar(
        "SELECT ws.handle_state_json FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id
         WHERE c.id=?1 AND ws.id=?2 AND ws.card_id=c.id AND ws.track_id=c.track_id
           AND ws.mode='resumable' AND ws.provider IN ('codex','claude','opencode')
           AND ws.state IN ('starting','running','idle','turn_pending')",
    )
    .bind(card_id)
    .bind(worker_session_id)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    let original_snapshot = original_snapshot.ok_or_else(|| {
        CalmError::Conflict("conversation input carrier changed or has no checkpoint")
    })?;
    let snapshot: Value = serde_json::from_str(&original_snapshot)?;
    validate_v2_frame(&snapshot)?;
    let checkpoint = serde_json::from_value(
        snapshot
            .get(CONVERSATION_INPUT_CHECKPOINT_KEY)
            .ok_or_else(|| CalmError::Conflict("canonical input checkpoint is missing"))?
            .clone(),
    )?;
    let carrier = CurrentInputCarrier {
        card_id: card_id.into(),
        worker_session_id: worker_session_id.into(),
        checkpoint,
        snapshot,
        original_snapshot,
    };
    carrier.checkpoint.validate().map_err(CalmError::Conflict)?;
    carrier.phase()?;
    Ok(carrier)
}

/// No public raw checkpoint writer: only the canonical acceptance/claim/receipt
/// owners in this module can change it. Recheck the original snapshot as well as
/// active identity, so a stale in-transaction view cannot overwrite another write.
pub(super) async fn save_input_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
) -> Result<()> {
    carrier.checkpoint.validate().map_err(CalmError::Conflict)?;
    carrier.snapshot[CONVERSATION_INPUT_CHECKPOINT_KEY] =
        serde_json::to_value(&carrier.checkpoint)?;
    validate_v2_frame(&carrier.snapshot)?;
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id
          WHERE c.id=?1 AND ws.id=?2 AND ws.card_id=c.id AND ws.track_id=c.track_id
            AND ws.state IN ('starting','running','idle','turn_pending') AND ws.handle_state_json=?3)",
    ).bind(&carrier.card_id).bind(&carrier.worker_session_id).bind(&carrier.original_snapshot).fetch_one(&mut **tx).await?;
    if !current {
        return Err(CalmError::Conflict("canonical input checkpoint changed"));
    }
    let written = super::super::session_set_handle_state_tx(
        tx,
        &carrier.worker_session_id,
        Some(carrier.snapshot.clone()),
    )
    .await
    .map_err(|error| CalmError::Internal(error.to_string()))?;
    if !written {
        return Err(CalmError::Conflict("canonical input carrier retired"));
    }
    carrier.original_snapshot = serde_json::to_string(&carrier.snapshot)?;
    Ok(())
}

fn validate_v2_frame(snapshot: &Value) -> Result<()> {
    if snapshot.get("schema_version").and_then(Value::as_u64) != Some(2)
        || snapshot.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE)
    {
        return Err(CalmError::Conflict(
            "conversation requires reconciled V2 input storage",
        ));
    }
    for retired in [
        "pending_queue",
        "pending_envelope_ids",
        "pending_entry_meta",
        "pending_message_ids",
        "issued_input_segments",
        "projection_client_id",
    ] {
        if snapshot.get(retired).is_some() {
            return Err(CalmError::Conflict(
                "V2 input checkpoint contains a retired body or batch authority",
            ));
        }
    }
    Ok(())
}
