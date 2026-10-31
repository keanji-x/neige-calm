//! System Assistant acceptance and permanent original ACKs; no execution authority.
use crate::error::{CalmError, Result};
use calm_types::{
    conversation::AcceptedInputRef, event::Event, ids::ActorId, observation::Observation,
    system_assistant::*,
};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Sqlite, Transaction};

#[derive(FromRow)]
struct Entry {
    track_id: String,
    original_worker: String,
    source: String,
    creation_floor: i64,
    bootstrap_digest: String,
}
fn conflict() -> CalmError {
    CalmError::Conflict("System receipt must bind original targeted acceptance")
}
fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Register the original empty carrier. Floor and worker are read, never caller facts.
/// The caller owns allocation/source qualification and commits this with acceptance.
pub async fn system_assistant_entry_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    source: SystemAssistantSource,
    bootstrap: &str,
) -> Result<()> {
    if assistant_key_used_tx(tx, card).await? {
        return Err(CalmError::IdempotencyKeyExhausted(
            "Assistant key is exhausted".into(),
        ));
    }
    let carrier = super::current_input_content_carrier_tx(tx, card).await?;
    let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(carrier.snapshot().clone())?;
    let floor = super::super::event_inputs::event_creation_floor_tx(tx).await?;
    if frame != calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(floor) {
        return Err(conflict());
    }
    sqlx::query("INSERT INTO conversation_system_assistant_entries(card_id,track_id,original_worker,source,creation_floor,bootstrap_digest)
        SELECT c.id,c.track_id,c.session_id,?2,?3,?4 FROM cards c WHERE c.id=?1")
        .bind(card).bind(source.namespace()).bind(floor).bind(text_digest(bootstrap))
        .execute(&mut **tx).await?;
    Ok(())
}

/// Exact original ACK, including after resource/event deletion; read-only evidence,
/// never permission to enqueue, replace a carrier, or execute again.
pub async fn system_assistant_receipt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    source: SystemAssistantSource,
    subject: &str,
) -> Result<Option<SystemAssistantReceipt>> {
    let receipt = sqlx::query_scalar::<_, String>(
        "SELECT r.receipt_json FROM conversation_system_assistant_receipts r
        JOIN conversation_system_assistant_entries e ON e.card_id=r.card_id AND e.source=r.source
        WHERE r.card_id=?1 AND r.source=?2 AND r.subject=?3",
    )
    .bind(card)
    .bind(source.namespace())
    .bind(subject)
    .fetch_optional(&mut **tx)
    .await?
    .map(|json| serde_json::from_str::<SystemAssistantReceipt>(&json).map_err(CalmError::from))
    .transpose()?;
    if let Some(original) = &receipt {
        let kind_matches_source = match (original.source, &original.acceptance) {
            (
                SystemAssistantSource::TodaySummary,
                SystemAssistantReceiptKind::Bootstrap { .. } | SystemAssistantReceiptKind::Input,
            ) => true,
            (SystemAssistantSource::WorkerWatcher, kind) => {
                kind.quiet_observation().is_some_and(|fact| {
                    fact.version == 1
                        && fact.track_id == original.track_id
                        && (kind.is_bootstrap() || fact.subject() == original.subject)
                })
            }
            _ => false,
        };
        if original.source != source
            || original.subject != subject
            || original.card_id != card
            || !kind_matches_source
        {
            return Err(conflict());
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE id=?1)")
            .bind(original.event_id)
            .fetch_one(&mut **tx)
            .await?;
        if exists
            && super::super::event_inputs::event_input_tx(tx, original.event_id)
                .await?
                .fingerprint
                != original.event_fingerprint
        {
            return Err(CalmError::IdempotencyKeyReused(
                "System input event changed".into(),
            ));
        }
    }
    Ok(receipt)
}

/// Source-scoped operation identity for callers whose first acceptance freezes
/// server-produced text. Resolve via permanent entries, then validate the same ACK.
pub async fn system_assistant_subject_receipt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: SystemAssistantSource,
    subject: &str,
) -> Result<Option<SystemAssistantReceipt>> {
    let cards: Vec<String> = sqlx::query_scalar(
        "SELECT r.card_id FROM conversation_system_assistant_receipts r JOIN conversation_system_assistant_entries e ON e.card_id=r.card_id AND e.source=r.source WHERE r.source=?1 AND r.subject=?2 LIMIT 2",
    ).bind(source.namespace()).bind(subject).fetch_all(&mut **tx).await?;
    match cards.as_slice() {
        [] => Ok(None),
        [card] => system_assistant_receipt_tx(tx, card, source, subject).await,
        _ => Err(conflict()),
    }
}

/// Validate only an already permanent original ACK. A read result is never a
/// capability to insert a receipt or accept an input, even after rollback.
pub async fn system_assistant_receipt_replay_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    source: SystemAssistantSource,
    subject: &str,
    event_id: i64,
    input_ref: &AcceptedInputRef,
) -> Result<SystemAssistantReceipt> {
    let original = system_assistant_receipt_tx(tx, card, source, subject)
        .await?
        .ok_or_else(conflict)?;
    if original.event_id != event_id
        || !matches!(input_ref, AcceptedInputRef::System { observation_id } if observation_id.as_str() == original.observation_id)
    {
        return Err(conflict());
    }
    Ok(original)
}

/// Load current SQL facts, qualify the source, and atomically accept and freeze
/// its first ACK using the sole generic acceptance writer. No caller snapshot,
/// pending, receipt, observation or time is accepted. On error the nested
/// transaction rolls back even when the caller catches the error and commits.
pub async fn system_assistant_accept_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    event_id: i64,
) -> Result<(super::AcceptedPendingInput, SystemAssistantReceipt)> {
    accept_with_source_tx(tx, card, event_id, None).await
}

/// Trusted SQL composition interface, not an HTTP or renderer capability. The
/// server's private live issuer owns screen qualification before calling here.
/// All independently SQL/event-derived facts are checked again by this owner.
pub async fn worker_watcher_accept_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    event_id: i64,
    observation: &QuietObservationFact,
) -> Result<(super::AcceptedPendingInput, SystemAssistantReceipt)> {
    accept_with_source_tx(tx, card, event_id, Some(observation)).await
}

async fn accept_with_source_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    event_id: i64,
    observation: Option<&QuietObservationFact>,
) -> Result<(super::AcceptedPendingInput, SystemAssistantReceipt)> {
    let mut savepoint =
        <&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite> as ::sqlx::Acquire>::begin(tx).await?;
    let result = accept_original_tx(&mut savepoint, card, event_id, observation).await;
    match result {
        Ok(result) => {
            savepoint.commit().await?;
            Ok(result)
        }
        Err(error) => {
            savepoint.rollback().await?;
            Err(error)
        }
    }
}

async fn accept_original_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    event_id: i64,
    observation: Option<&QuietObservationFact>,
) -> Result<(super::AcceptedPendingInput, SystemAssistantReceipt)> {
    let entry = sqlx::query_as::<_, Entry>("SELECT e.track_id,e.original_worker,e.source,e.creation_floor,e.bootstrap_digest
        FROM conversation_system_assistant_entries e
        JOIN cards c ON c.id=e.card_id AND c.track_id=e.track_id AND c.session_id=e.original_worker
        JOIN worker_sessions ws ON ws.id=e.original_worker AND ws.card_id=c.id AND ws.track_id=c.track_id
        WHERE e.card_id=?1 AND c.role='assistant' AND ws.contract='assistant' AND ws.provider='codex'
          AND json_extract(c.payload,'$.harness_profile')='assistant'
          AND json_extract(c.payload,'$.assistant_provider')='codex'")
        .bind(card).fetch_optional(&mut **tx).await?.ok_or_else(conflict)?;
    let fact = super::super::event_inputs::event_input_tx(tx, event_id).await?;
    let Event::AssistantInputRequested {
        card_id,
        track_id,
        original_worker,
        source,
        subject,
        version,
        text,
    } = &fact.envelope.event
    else {
        return Err(conflict());
    };
    let area = super::conversation_area_tx(tx, card).await?;
    if card_id.as_str() != card
        || track_id.as_str() != entry.track_id
        || original_worker != &entry.original_worker
        || source.namespace() != entry.source
        || *version != 1
        || subject.is_empty()
        || event_id <= entry.creation_floor
        || !matches!(
            fact.envelope.actor,
            ActorId::Kernel | ActorId::KernelDispatcher
        )
        || fact.envelope.scope.card_id() != Some(card_id)
        || fact.envelope.scope.track_id() != Some(track_id)
        || fact.envelope.scope.area_id() != Some(&area.id)
    {
        return Err(conflict());
    }
    let bootstrap = subject == source.bootstrap_subject();
    if bootstrap && text_digest(text) != entry.bootstrap_digest {
        return Err(conflict());
    }
    let acceptance = match (source, observation, bootstrap) {
        (SystemAssistantSource::TodaySummary, None, true) => {
            SystemAssistantReceiptKind::Bootstrap {
                creation_floor: entry.creation_floor,
            }
        }
        (SystemAssistantSource::TodaySummary, None, false) => SystemAssistantReceiptKind::Input,
        (SystemAssistantSource::WorkerWatcher, Some(observed), bootstrap) => {
            validate_quiet_fact_tx(
                tx,
                observed,
                &entry.track_id,
                area.id.as_str(),
                subject,
                bootstrap,
            )
            .await?;
            if !bootstrap {
                let has_body: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_system_assistant_receipts WHERE card_id=?1 AND bootstrap=0)")
                    .bind(card).fetch_one(&mut **tx).await?;
                let first =
                    system_assistant_receipt_tx(tx, card, *source, source.bootstrap_subject())
                        .await?
                        .ok_or_else(conflict)?;
                if !has_body && first.acceptance.quiet_observation() != Some(observed) {
                    return Err(conflict());
                }
            }
            if bootstrap {
                SystemAssistantReceiptKind::WatcherBootstrap {
                    creation_floor: entry.creation_floor,
                    observation: observed.clone(),
                }
            } else {
                SystemAssistantReceiptKind::WatcherInput {
                    observation: observed.clone(),
                }
            }
        }
        _ => return Err(conflict()),
    };
    if system_assistant_receipt_tx(tx, card, *source, subject)
        .await?
        .is_some()
    {
        return Err(CalmError::IdempotencyKeyReused(
            "System input key names another acceptance".into(),
        ));
    }
    let mut carrier = super::current_input_carrier_tx(tx, card, &entry.original_worker).await?;
    let observation =
        calm_types::conversation::SystemObservation::try_from(Observation::SystemContext {
            text: text.clone(),
        })
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    let accepted = super::accept_pending_system_tx(tx, &mut carrier, observation, &fact).await?;
    let receipt =
        record_first_receipt_tx(tx, card, entry, &fact, &accepted, &carrier, acceptance).await?;
    Ok((accepted, receipt))
}

async fn validate_quiet_fact_tx(
    tx: &mut Transaction<'_, Sqlite>,
    observed: &QuietObservationFact,
    track: &str,
    area: &str,
    subject: &str,
    bootstrap: bool,
) -> Result<()> {
    let database: String = sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton=1")
        .fetch_one(&mut **tx)
        .await?;
    if observed.version != 1
        || observed.database_id != database
        || observed.track_id != track
        || observed.area_id != area
        || observed.renderer_id.is_empty()
        || observed.last_output_ms <= 0
        || observed.quiet_after_ms <= 0
        || observed
            .observed_at_ms
            .checked_sub(observed.last_output_ms)
            .is_none_or(|age| age < observed.quiet_after_ms)
        || (!bootstrap && subject != observed.subject())
    {
        return Err(conflict());
    }
    let task = super::super::task_current_get_tx(tx, track, &observed.task_key)
        .await?
        .ok_or_else(conflict)?;
    if task.id != observed.attempt_id
        || task.worker_card_id.as_deref() != Some(&observed.worker_card_id)
        || task.status != crate::model::TaskStatus::Running
        || !matches!(
            task.kind,
            crate::model::TaskKind::Codex | crate::model::TaskKind::Claude
        )
        || task.spawn == calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE
    {
        return Err(conflict());
    }
    let terminal = super::super::terminal_get_by_card_tx(tx, &observed.worker_card_id)
        .await?
        .ok_or_else(conflict)?;
    let session = super::super::session_get_tx(tx, &observed.worker_session_id.clone().into())
        .await?
        .ok_or_else(conflict)?;
    let current =
        super::super::session_projection_projectable_for_card_tx(tx, &observed.worker_card_id)
            .await?
            .ok_or_else(conflict)?;
    let card = super::super::card_for_update_tx(tx, &observed.worker_card_id).await?;
    let role: String = sqlx::query_scalar("SELECT role FROM cards WHERE id=?1")
        .bind(&observed.worker_card_id)
        .fetch_one(&mut **tx)
        .await?;
    let operation = match session.spawn_op_id.as_deref() {
        Some(op) => crate::worker_binding::operation_idempotency_key(&mut **tx, op).await?,
        None => None,
    };
    let expected_kind = match task.kind {
        crate::model::TaskKind::Codex => calm_types::runtime::WorkerSessionKind::CodexCard,
        crate::model::TaskKind::Claude => calm_types::runtime::WorkerSessionKind::ClaudeCard,
        _ => return Err(conflict()),
    };
    if current.kind != expected_kind
        || terminal.id != observed.terminal_id
        || current.id != observed.worker_session_id
        || current.terminal_run_id.as_deref() != Some(&observed.terminal_id)
        || session.terminal_run_id.as_deref() != Some(&observed.terminal_id)
        || session.card_id.as_ref().map(|id| id.as_str()) != Some(&observed.worker_card_id)
        || session.track_id.as_str() != track
        || card.track_id.as_str() != track
        || role != "worker"
        || card.kind
            != match task.kind {
                crate::model::TaskKind::Codex => "codex",
                crate::model::TaskKind::Claude => "claude",
                _ => return Err(conflict()),
            }
        || !session.state.is_active_authority()
        || operation.as_deref() != Some(&observed.attempt_id)
    {
        return Err(conflict());
    }
    Ok(())
}

// This write is private to the atomic owner; no retained public read result can
// be fed back here. All qualification precedes the sole acceptance writer.
async fn record_first_receipt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    entry: Entry,
    fact: &super::super::event_inputs::EventInputFact,
    accepted: &super::AcceptedPendingInput,
    carrier: &super::CurrentInputCarrier,
    acceptance: SystemAssistantReceiptKind,
) -> Result<SystemAssistantReceipt> {
    let Event::AssistantInputRequested {
        source,
        subject,
        text,
        ..
    } = &fact.envelope.event
    else {
        return Err(conflict());
    };
    let AcceptedInputRef::System { observation_id } = &accepted.input_ref else {
        return Err(conflict());
    };
    let queued_at_ms = carrier
        .checkpoint()
        .pending
        .last()
        .ok_or_else(conflict)?
        .queued_at_ms();
    let sequence = accepted.presentation_sequence;
    let event_id = fact.envelope.id;
    let receipt = SystemAssistantReceipt {
        version: SystemAssistantReceiptVersion::SystemAssistantV1,
        source: *source,
        subject: subject.clone(),
        card_id: card.into(),
        track_id: entry.track_id,
        original_worker: entry.original_worker,
        event_id,
        event_fingerprint: fact.fingerprint.clone(),
        observation_id: observation_id.as_str().into(),
        queued_at_ms,
        presentation_sequence: sequence,
        payload_digest: text_digest(text),
        acceptance,
    };
    sqlx::query("INSERT INTO conversation_system_assistant_receipts(card_id,source,subject,event_id,observation_id,bootstrap,receipt_json) VALUES(?1,?2,?3,?4,?5,?6,?7)")
        .bind(card).bind(source.namespace()).bind(subject).bind(event_id).bind(observation_id.as_str())
        .bind(receipt.acceptance.is_bootstrap())
        .bind(serde_json::to_string(&receipt)?).execute(&mut **tx).await?;
    Ok(receipt)
}

/// Shared spent namespace evidence. It grants no execution or replay authority.
pub async fn assistant_key_used_tx(tx: &mut Transaction<'_, Sqlite>, card: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_assistant_creations WHERE card_id=?1) OR EXISTS(SELECT 1 FROM conversation_system_assistant_entries WHERE card_id=?1)")
        .bind(card).fetch_one(&mut **tx).await?)
}

#[cfg(test)]
mod tests;
