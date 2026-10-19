//! Read-only proof of the start owner's committed transfer, never permission to rewrite its source.
use super::*;
use crate::model::HarnessQueueEntryRef;
use crate::operation::PhaseTag;

pub(crate) struct CommittedInputTransfer(());

/// One SELECT observes the source's retirement/stamp, the card's successor and the start
/// journal in the same SQLite snapshot. A provider response, missing row or retirement alone
/// proves nothing. Compensation remains the start owner's responsibility; this proof only
/// ends the predecessor's result checkpoint obligation, not the old MOVE/redelivery window.
pub(crate) async fn committed_transfer(
    repo: &dyn Repo,
    card_id: &str,
    source_id: &str,
    entry: &HarnessQueueEntryRef,
) -> Result<Option<CommittedInputTransfer>> {
    let db = repo.sqlite_db().ok_or_else(|| {
        CalmError::Internal("input transfer proof requires the SQLite owner facts".into())
    })?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT o.phase, o.tx_output_json FROM worker_sessions source \
         JOIN cards c ON c.id = source.card_id \
         JOIN worker_sessions successor ON successor.id = c.session_id \
         JOIN operations o ON json_extract(o.tx_output_json, ?3) = successor.id \
         WHERE source.id = ?1 AND source.card_id = ?2 AND source.state = 'superseded' \
         AND source.queue_harvested_at_ms IS NOT NULL \
         AND successor.id <> source.id AND successor.card_id = source.card_id \
         AND o.kind = 'planner-harness-start' \
         AND json_extract(o.tx_output_json, '$.data.card_id') = c.id \
         AND json_extract(o.tx_output_json, ?4) = source.id",
    )
    .bind(source_id)
    .bind(card_id)
    .bind(format!("$.data.{SUCCESSOR_SESSION_KEY}"))
    .bind(format!("$.data.{PREDECESSOR_SESSION_KEY}"))
    .fetch_all(db.read_pool())
    .await?;
    for (phase, output) in rows {
        if !matches!(
            PhaseTag::from_db_str(&phase)?,
            PhaseTag::TxCommitted
                | PhaseTag::AppServerInteract
                | PhaseTag::AwaitingRetry
                | PhaseTag::SpawnStarted
                | PhaseTag::SpawnSucceeded
                | PhaseTag::Parked
                | PhaseTag::Succeeded
        ) {
            continue;
        }
        let output: TxOutput = serde_json::from_str(&output)?;
        let transferred = read_harvested_from_journal(&output)?;
        if transferred.iter().any(|source| {
            source.worker_session_id == source_id
                && source.messages.iter().any(|message| {
                    message
                        .entry_ref
                        .as_ref()
                        .is_some_and(|moved| moved == entry)
                })
        }) {
            return Ok(Some(CommittedInputTransfer(())));
        }
    }
    Ok(None)
}
