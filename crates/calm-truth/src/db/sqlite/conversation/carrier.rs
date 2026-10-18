//! The existing worker-session snapshot is the sole pending authority. Every
//! canonical mutation loads and fences its exact current carrier in the caller's
//! transaction, then publishes only the committed snapshot to runtime caches.
use crate::error::{CalmError, Result};
use calm_types::conversation::{CONVERSATION_INPUT_CHECKPOINT_KEY, ConversationInputCheckpoint};
use calm_types::harness::HarnessPhaseTag;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use serde_json::Value;
use sqlx::{Sqlite, Transaction};

pub struct CurrentInputCarrier {
    pub(super) card_id: String,
    pub(super) worker_session_id: String,
    pub(super) checkpoint: ConversationInputCheckpoint,
    pub(super) snapshot: Value,
    original_snapshot: String,
    execution_current: bool,
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

    /// Factual row state, not authority to start another operation.
    pub fn is_execution_current(&self) -> bool {
        self.execution_current
    }

    pub(super) fn require_execution_current(&self) -> Result<()> {
        if !self.execution_current {
            return Err(CalmError::Conflict(
                "failed carrier permits content only, not execution",
            ));
        }
        Ok(())
    }

    /// Read the phase from this protected carrier; this fact grants no execution.
    pub fn phase(&self) -> Result<HarnessPhaseTag> {
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
    load_input_carrier_tx(tx, card_id, worker_session_id, false).await
}

/// Recording a returned native fact is allowed on the same current failed
/// carrier. Exact worker binding remains mandatory; claims/crossings still use
/// the active-only loader and require_execution_current.
pub async fn current_input_fact_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<CurrentInputCarrier> {
    load_input_carrier_tx(tx, card_id, worker_session_id, true).await
}

/// Content remains saveable on the actual current failed carrier. This grants
/// no claim/marker/phase transition and never picks a historical replacement.
pub async fn current_input_content_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<CurrentInputCarrier> {
    let worker: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    let worker = worker
        .ok_or_else(|| CalmError::Conflict("accepted conversation has no current carrier"))?;
    load_input_carrier_tx(tx, card_id, &worker, true).await
}

async fn load_input_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
    content: bool,
) -> Result<CurrentInputCarrier> {
    let row: Option<(String,Option<String>)> = sqlx::query_as(
        "SELECT ws.state, ws.handle_state_json FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id
         WHERE c.id=?1 AND ws.id=?2 AND ws.card_id=c.id AND ws.track_id=c.track_id
           AND ws.mode='resumable' AND ws.provider IN ('codex','claude','opencode')
           AND (ws.state IN ('starting','running','idle','turn_pending') OR (?3 AND ws.state='failed'))",
    )
    .bind(card_id)
    .bind(worker_session_id)
    .bind(content)
    .fetch_optional(&mut **tx)
    .await?;
    let (state, original_snapshot) =
        row.ok_or_else(|| CalmError::Conflict("conversation has no current content carrier"))?;
    let original_snapshot = original_snapshot.ok_or_else(|| {
        CalmError::Conflict("conversation input carrier changed or has no checkpoint")
    })?;
    let snapshot: Value = serde_json::from_str(&original_snapshot)?;
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(snapshot.clone())?;
    let checkpoint = frame.canonical_inputs;
    let carrier = CurrentInputCarrier {
        card_id: card_id.into(),
        worker_session_id: worker_session_id.into(),
        checkpoint,
        snapshot,
        original_snapshot,
        execution_current: state != "failed",
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
    let _: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot.clone())?;
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id
          WHERE c.id=?1 AND ws.id=?2 AND ws.card_id=c.id AND ws.track_id=c.track_id
            AND (ws.state IN ('starting','running','idle','turn_pending') OR (?4 AND ws.state='failed')) AND ws.handle_state_json=?3)",
    ).bind(&carrier.card_id).bind(&carrier.worker_session_id).bind(&carrier.original_snapshot).bind(!carrier.execution_current).fetch_one(&mut **tx).await?;
    if !current {
        return Err(CalmError::Conflict("canonical input checkpoint changed"));
    }
    if !carrier.execution_current {
        super::super::session_set_handle_state_of_any_runtime_tx(
            tx,
            &carrier.worker_session_id,
            Some(carrier.snapshot.clone()),
            crate::model::now_ms(),
        )
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))?;
        carrier.original_snapshot = serde_json::to_string(&carrier.snapshot)?;
        return Ok(());
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
