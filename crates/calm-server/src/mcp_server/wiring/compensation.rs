//! Undo only the credential revoked by this start's replacement of a loaded Codex Assistant.
use super::*;
use crate::db::sqlite::session_restore_from_superseded_runtime_tx;
use crate::session_projection_repo::WorkerSessionState;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CodexAssistantCredential {
    worker_session_id: String,
    replacement_worker_session_id: String,
    thread_id: String,
    hash: String,
}

/// Capture in the transaction that retires the current carrier. Hashes never authorize the
/// successor, and the journal contains no plaintext. A failed/stale carrier is not captured.
pub(crate) async fn capture_codex_assistant_credential_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker_session_id: &str,
    replacement_worker_session_id: &str,
) -> Result<Option<CodexAssistantCredential>> {
    let row: Option<(String, String)> = sqlx::query_as(
        r#"SELECT s.thread_id, s.mcp_token_hash FROM worker_sessions s
           JOIN cards c ON c.id=s.card_id AND c.session_id=s.id AND c.track_id=s.track_id
           WHERE s.id=?1 AND s.contract='assistant' AND s.provider='codex'
             AND s.mode='resumable' AND s.state IN ('starting','idle','running','turn_pending')
             AND s.thread_id IS NOT NULL AND s.mcp_token_hash IS NOT NULL
             AND c.kind='codex' AND c.role='assistant'
             AND json_extract(c.payload,'$.harness_profile')='assistant'
             AND json_extract(c.payload,'$.assistant_provider')='codex'"#,
    )
    .bind(worker_session_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(thread_id, hash)| CodexAssistantCredential {
        worker_session_id: worker_session_id.into(),
        replacement_worker_session_id: replacement_worker_session_id.into(),
        thread_id,
        hash,
    }))
}

/// Restore the exact predecessor and its loaded thread's credential atomically. A later
/// current runtime, a rotated credential, or a live replacement makes this undo inapplicable.
pub(crate) async fn restore_codex_assistant_credential_tx(
    tx: &mut Transaction<'_, Sqlite>,
    credential: &CodexAssistantCredential,
    predecessor: &str,
    status: WorkerSessionState,
) -> Result<()> {
    if credential.worker_session_id != predecessor || !status.is_active_authority() {
        return Err(CalmError::Conflict(
            "Invalid Codex credential compensation".into(),
        ));
    }
    let eligible: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM worker_sessions old
           JOIN cards c ON c.id=old.card_id AND c.track_id=old.track_id
           JOIN worker_sessions next ON next.card_id=old.card_id AND next.track_id=old.track_id
           WHERE old.id=?1 AND next.id=?2 AND old.id<>next.id AND old.thread_id=?3
             AND ((old.state='superseded' AND c.session_id=next.id AND old.mcp_token_hash IS NULL)
                  OR (old.state=?4 AND c.session_id=old.id AND old.mcp_token_hash=?5))
             AND next.state='failed' AND next.mcp_token_hash IS NULL
             AND old.contract='assistant' AND next.contract=old.contract
             AND old.provider='codex' AND next.provider=old.provider
             AND old.mode='resumable' AND next.mode=old.mode
             AND c.kind='codex' AND c.role='assistant'
             AND json_extract(c.payload,'$.harness_profile')='assistant'
             AND json_extract(c.payload,'$.assistant_provider')='codex')"#,
    )
    .bind(predecessor)
    .bind(&credential.replacement_worker_session_id)
    .bind(&credential.thread_id)
    .bind(status.as_db_str())
    .bind(&credential.hash)
    .fetch_one(&mut **tx)
    .await?;
    if !eligible {
        return Err(CalmError::Conflict(
            "Codex credential compensation owner changed".into(),
        ));
    }
    session_restore_from_superseded_runtime_tx(tx, &predecessor.to_string(), status).await?;
    // The same carrier, not the replacement, is current again in this transaction.
    session_mcp_token_set_if_active_tx(tx, predecessor, &credential.hash).await?;
    let card_id = credential_card_id(tx, predecessor).await?;
    card_mcp_token_set_tx(tx, &card_id, &credential.hash).await?;
    Ok(())
}

async fn credential_card_id(
    tx: &mut Transaction<'_, Sqlite>,
    worker_session_id: &str,
) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
            .bind(worker_session_id)
            .fetch_one(&mut **tx)
            .await?,
    )
}
