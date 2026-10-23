//! Durable command operation submission, bounded infrastructure retries and owner wake.
use super::*;
use crate::operation::{
    OperationKey, OperationRuntime,
    objective_check_run::{self, Payload},
};
use calm_truth::objective_host::StoredIntent;
use serde_json::Value;
use std::sync::LazyLock;

static DISPATCH: LazyLock<crate::per_card_lock::KeyedLocks> =
    LazyLock::new(crate::per_card_lock::new_keyed_locks);
const ATTEMPTS: usize = 3;

pub(super) async fn lock(track: &str) -> crate::per_card_lock::KeyedLockGuard {
    crate::per_card_lock::lock_key(&DISPATCH, track).await
}
pub(super) async fn dispatch(
    repo: &dyn RepoEventWrite,
    db: &calm_truth::db::sqlite::SqliteDb,
    bus: &EventBus,
    runtime: Option<&OperationRuntime>,
    row: &StoredIntent,
) -> Result<bool> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(db).await?;
    let current = objective_host::current_check_tx(&mut tx, &row.id).await?;
    tx.commit().await?;
    let Some((_, definition)) = current else {
        return Ok(false);
    };
    if !matches!(definition.config, CheckConfig::Command(_)) {
        return Ok(false);
    }
    let Some(runtime) = runtime else {
        return Ok(true);
    };
    // Readiness is a deferral, never a durable execution failure. Boot/tick
    // reconciliation retries the same intent after the engine has been bound.
    if runtime.require_gate_execution().is_err() {
        return Ok(true);
    }
    let prefix = format!("objective-check:{}", row.id);
    let runs: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT phase,COALESCE(json_extract(tx_output_json,'$.result.reason'),last_error) FROM \
         operations WHERE kind=? AND json_extract(payload_json,'$.intent_id.track_id')=? AND \
         json_extract(payload_json,'$.intent_id.seq')=? AND \
         json_extract(payload_json,'$.intent_id.ordinal')=? ORDER BY created_at_ms,id",
    )
    .bind(objective_check_run::KIND)
    .bind(row.id.track_id.as_str())
    .bind(row.id.seq.0 as i64)
    .bind(i64::from(row.id.ordinal))
    .fetch_all(db.read_pool())
    .await?;
    if runs
        .iter()
        .any(|(phase, _)| !matches!(phase.as_str(), "succeeded" | "failed"))
    {
        return Ok(true);
    }
    if runs.len() >= ATTEMPTS {
        let reason = runs
            .last()
            .and_then(|(_, e)| e.as_deref())
            .unwrap_or("command timed out or execution infrastructure failed")
            .to_owned();
        let id = row.id.clone();
        let envelope = crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
            let status: String = sqlx::query_scalar("SELECT status FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?")
                .bind(id.track_id.as_str()).bind(id.seq.0 as i64).bind(i64::from(id.ordinal)).fetch_one(&mut **tx).await?;
            if status == "settled" { return Ok(None); }
            objective_host::intent_settle_tx(tx, &id, ExecutorSettlement::Failed { reason: reason.clone() }).await?;
            let track = track_get_tx(tx, &id.track_id).await?;
            let event = Event::TrackWakeRequested {
                track_id: id.track_id.clone(), source: objective_check_run::KIND.into(), key: id.to_string(),
                text: include_str!("../../prompts/objective_check_failed.md").trim_end().replace("{intent}", &id.to_string()).replace("{reason}", &reason),
            };
            let scope = EventScope::Track { track: track.id, area: track.area_id };
            let event_id = append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event).await?;
            Ok(Some(BroadcastEnvelope { id: event_id, event_version: SYNC_EVENT_VERSION, actor: ActorId::Kernel, scope, event }))
        })).await?;
        if let Some(envelope) = envelope {
            bus.emit_envelope(envelope);
        }
        return Ok(true);
    }
    // Submission is serial per track. Pending rows have no target_id yet, so use
    // the adapter's typed payload binding for the occupancy query.
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE kind=? AND \
         json_extract(payload_json,'$.intent_id.track_id')=? AND phase NOT IN ('succeeded','failed'))"
    ).bind(objective_check_run::KIND).bind(row.id.track_id.as_str()).fetch_one(db.read_pool()).await?;
    if busy {
        return Ok(true);
    }
    let key = if runs.is_empty() {
        prefix
    } else {
        format!("{prefix}#{}", runs.len() + 1)
    };
    let payload: Value = serde_json::to_value(Payload {
        intent_id: row.id.clone(),
    })?;
    runtime
        .submit(
            objective_check_run::KIND,
            OperationKey {
                operation_key: key.clone(),
                idempotency_key: Some(key),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)?,
            },
            payload,
        )
        .await?;
    Ok(true)
}
