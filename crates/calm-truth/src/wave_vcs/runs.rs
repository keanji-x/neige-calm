use crate::db::WaveEvent;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, WaveId};
use crate::model::Card;
use crate::wave_fs_dto::WaveFsRunStatus;
use crate::wave_fs_view::{self, RunEventProjection, RunProjection, RunVerdictProjection};
use serde_json::Value;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

use super::snapshot::card_projection_from_row;
use super::types::{CardProjection, CardVisibility};

pub(super) type EventRow = (
    i64,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn run_events_for_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Vec<WaveEvent>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_cove, scope_wave, scope_card
           FROM events
           WHERE scope_wave = ?1
             AND kind IN (
               'codex.worker_requested',
               'terminal.worker_requested',
               'task.dispatched',
               'task.completed',
               'task.failed'
             )
           ORDER BY id ASC"#,
    )
    .bind(wave_id.as_str())
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter().map(wave_event_from_row).collect()
}

pub(super) fn wave_event_from_row(row: EventRow) -> Result<WaveEvent> {
    let (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) = row;
    let payload = serde_json::from_str(&payload_text)?;
    let actor = serde_json::from_str::<ActorId>(&actor_text)?;
    let scope = EventScope::from_row(
        sk.as_deref(),
        sc.as_deref(),
        sw.as_deref(),
        scard.as_deref(),
    );
    let event = Event::from_kind_and_payload(&kind, payload)?;
    Ok(WaveEvent {
        id,
        at,
        actor,
        scope,
        event,
    })
}

pub(super) async fn project_runs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    cards: &[CardProjection],
) -> Result<Vec<RunProjection>> {
    let events = run_events_for_wave_tx(tx, wave_id).await?;

    let mut keys = BTreeSet::new();
    let mut worker_cards = BTreeMap::new();
    for card in cards.iter().cloned() {
        if card.role != "worker" {
            continue;
        }
        if let Some(key) = idempotency_key_from_payload(&card.card.payload) {
            keys.insert(key.to_string());
            worker_cards.entry(key.to_string()).or_insert(card.card);
        }
    }

    let mut requested = BTreeMap::<String, RunEventProjection>::new();
    let mut requested_kind = BTreeMap::<String, &'static str>::new();
    let mut dispatched = BTreeMap::<String, RunEventProjection>::new();
    let mut dispatched_kind = BTreeMap::<String, &'static str>::new();
    let mut completed = BTreeMap::<String, RunEventProjection>::new();
    let mut failed = BTreeMap::<String, RunEventProjection>::new();
    let mut verdict = BTreeMap::<String, RunEventProjection>::new();

    for row in events {
        match &row.event {
            Event::CodexWorkerRequested {
                idempotency_key, ..
            } => {
                keys.insert(idempotency_key.clone());
                requested_kind.insert(idempotency_key.clone(), "codex");
                record_earliest(
                    &mut requested,
                    idempotency_key,
                    run_event(
                        row.id,
                        row.at,
                        "codex.worker_requested",
                        row.event.payload_value(),
                    ),
                );
            }
            Event::TerminalWorkerRequested {
                idempotency_key, ..
            } => {
                keys.insert(idempotency_key.clone());
                requested_kind.insert(idempotency_key.clone(), "terminal");
                record_earliest(
                    &mut requested,
                    idempotency_key,
                    run_event(
                        row.id,
                        row.at,
                        "terminal.worker_requested",
                        row.event.payload_value(),
                    ),
                );
            }
            // Issue #644 PR-B — scheduler claim record; merged below as
            // the §5.6 requested-record fallback.
            Event::TaskDispatched {
                idempotency_key,
                kind,
                ..
            } => {
                keys.insert(idempotency_key.clone());
                dispatched_kind
                    .insert(idempotency_key.clone(), wave_fs_view::run_kind_static(kind));
                record_earliest(
                    &mut dispatched,
                    idempotency_key,
                    run_event(row.id, row.at, "task.dispatched", row.event.payload_value()),
                );
            }
            Event::TaskCompleted {
                idempotency_key, ..
            } => {
                let event = run_event(row.id, row.at, "task.completed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    record_latest(&mut verdict, idempotency_key, event);
                } else {
                    record_latest(&mut completed, idempotency_key, event);
                }
            }
            Event::TaskFailed {
                idempotency_key, ..
            } => {
                let event = run_event(row.id, row.at, "task.failed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    record_latest(&mut verdict, idempotency_key, event);
                } else {
                    record_latest(&mut failed, idempotency_key, event);
                }
            }
            _ => {}
        }
    }

    // §5.6 fallback: keys with a dispatch record but no
    // `*.worker_requested` event use it as their requested-record.
    for (key, event) in dispatched {
        requested.entry(key).or_insert(event);
    }
    for (key, kind) in dispatched_kind {
        requested_kind.entry(key).or_insert(kind);
    }

    Ok(keys
        .into_iter()
        .filter(|key| run_key_is_visible(key))
        .map(|key| {
            let worker_card = worker_cards.remove(&key);
            let requested_event = requested.remove(&key);
            let completed_event = completed.remove(&key);
            let failed_event = failed.remove(&key);
            let verdict_event = verdict.remove(&key);
            let verdict = verdict_event.as_ref().and_then(verdict_from_event);
            let final_event = latest_final_event(completed_event.as_ref(), failed_event.as_ref());
            let (status, finished_at) = match (requested_event.as_ref(), final_event) {
                (Some(_), Some(("completed", event))) => {
                    (WaveFsRunStatus::Completed, Some(event.at))
                }
                (Some(_), Some(("failed", event))) => (WaveFsRunStatus::Failed, Some(event.at)),
                (Some(_), Some((_, event))) => (WaveFsRunStatus::Unknown, Some(event.at)),
                (Some(_), None) if worker_card.is_some() => (WaveFsRunStatus::Running, None),
                (Some(_), None) => (WaveFsRunStatus::Requested, None),
                (None, _) => (WaveFsRunStatus::Unknown, None),
            };
            let kind = worker_card
                .as_ref()
                .and_then(run_kind_from_card)
                .or_else(|| requested_kind.get(&key).copied())
                .unwrap_or("unknown")
                .to_string();
            RunProjection {
                idempotency_key: key,
                status,
                kind,
                requested_at: requested_event.as_ref().map(|event| event.at),
                finished_at,
                worker_card,
                requested_event,
                completed_event,
                failed_event,
                verdict,
                verdict_event,
            }
        })
        .collect())
}

pub(super) async fn project_run_by_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
    card_visibility: &CardVisibility,
) -> Result<Option<RunProjection>> {
    if !run_key_is_visible(key) {
        return Ok(None);
    }
    let worker_projection = worker_card_for_run_key_tx(tx, wave_id, key, card_visibility).await?;
    let worker_card = worker_projection.map(|projection| projection.card);
    let events = run_events_for_key_tx(tx, wave_id, key).await?;
    if worker_card.is_none() && events.is_empty() {
        return Ok(None);
    }

    let mut requested_event = None;
    let mut requested_kind = None;
    let mut dispatched_event: Option<RunEventProjection> = None;
    let mut dispatched_kind = None;
    let mut completed_event = None;
    let mut failed_event = None;
    let mut verdict_event = None;

    for row in events {
        match &row.event {
            // Issue #644 PR-B — scheduler claim record; §5.6 fallback
            // applied after the loop when no `*.worker_requested` landed.
            Event::TaskDispatched { kind, .. } => {
                dispatched_kind = Some(wave_fs_view::run_kind_static(kind));
                let event = run_event(row.id, row.at, "task.dispatched", row.event.payload_value());
                if dispatched_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    dispatched_event = Some(event);
                }
            }
            Event::CodexWorkerRequested { .. } => {
                requested_kind = Some("codex");
                let event = run_event(
                    row.id,
                    row.at,
                    "codex.worker_requested",
                    row.event.payload_value(),
                );
                if requested_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    requested_event = Some(event);
                }
            }
            Event::TerminalWorkerRequested { .. } => {
                requested_kind = Some("terminal");
                let event = run_event(
                    row.id,
                    row.at,
                    "terminal.worker_requested",
                    row.event.payload_value(),
                );
                if requested_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    requested_event = Some(event);
                }
            }
            Event::TaskCompleted { .. } => {
                let event = run_event(row.id, row.at, "task.completed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    if verdict_event
                        .as_ref()
                        .is_none_or(|existing: &RunEventProjection| {
                            existing.event_id < event.event_id
                        })
                    {
                        verdict_event = Some(event);
                    }
                } else if completed_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id < event.event_id)
                {
                    completed_event = Some(event);
                }
            }
            Event::TaskFailed { .. } => {
                let event = run_event(row.id, row.at, "task.failed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    if verdict_event
                        .as_ref()
                        .is_none_or(|existing: &RunEventProjection| {
                            existing.event_id < event.event_id
                        })
                    {
                        verdict_event = Some(event);
                    }
                } else if failed_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id < event.event_id)
                {
                    failed_event = Some(event);
                }
            }
            _ => {}
        }
    }

    // §5.6 fallback: the dispatch record stands in for a missing
    // `*.worker_requested` event.
    if requested_event.is_none() {
        requested_event = dispatched_event;
    }
    if requested_kind.is_none() {
        requested_kind = dispatched_kind;
    }

    let verdict = verdict_event.as_ref().and_then(verdict_from_event);
    let final_event = latest_final_event(completed_event.as_ref(), failed_event.as_ref());
    let (status, finished_at) = match (requested_event.as_ref(), final_event) {
        (Some(_), Some(("completed", event))) => (WaveFsRunStatus::Completed, Some(event.at)),
        (Some(_), Some(("failed", event))) => (WaveFsRunStatus::Failed, Some(event.at)),
        (Some(_), Some((_, event))) => (WaveFsRunStatus::Unknown, Some(event.at)),
        (Some(_), None) if worker_card.is_some() => (WaveFsRunStatus::Running, None),
        (Some(_), None) => (WaveFsRunStatus::Requested, None),
        (None, _) => (WaveFsRunStatus::Unknown, None),
    };
    let kind = worker_card
        .as_ref()
        .and_then(run_kind_from_card)
        .or(requested_kind)
        .unwrap_or("unknown")
        .to_string();

    Ok(Some(RunProjection {
        idempotency_key: key.to_string(),
        status,
        kind,
        requested_at: requested_event.as_ref().map(|event| event.at),
        finished_at,
        worker_card,
        requested_event,
        completed_event,
        failed_event,
        verdict,
        verdict_event,
    }))
}

async fn worker_card_for_run_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
    visibility: &CardVisibility,
) -> Result<Option<CardProjection>> {
    let rows = sqlx::query(
        r#"SELECT id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_wave = cards.wave_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE wave_id = ?1
             AND role = 'worker'
             AND json_extract(payload, '$.idempotency_key') = ?2
           ORDER BY sort ASC, id ASC
           "#,
    )
    .bind(wave_id.as_str())
    .bind(key)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let id: String = row.try_get("id")?;
        let announced: i64 = row.try_get("vcs_announced")?;
        if visibility.includes(&id, announced != 0) {
            return card_projection_from_row(row).map(Some);
        }
    }
    Ok(None)
}

async fn run_events_for_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
) -> Result<Vec<WaveEvent>> {
    type EventRow = (
        i64,
        String,
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_cove, scope_wave, scope_card
           FROM events
           WHERE scope_wave = ?1
             AND kind IN (
               'codex.worker_requested',
               'terminal.worker_requested',
               'task.dispatched',
               'task.completed',
               'task.failed'
             )
             AND json_extract(payload, '$.idempotency_key') = ?2
           ORDER BY id ASC"#,
    )
    .bind(wave_id.as_str())
    .bind(key)
    .fetch_all(&mut **tx)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) in rows {
        let payload = serde_json::from_str(&payload_text)?;
        let actor = serde_json::from_str::<ActorId>(&actor_text)?;
        let scope = EventScope::from_row(
            sk.as_deref(),
            sc.as_deref(),
            sw.as_deref(),
            scard.as_deref(),
        );
        let event = Event::from_kind_and_payload(&kind, payload)?;
        out.push(WaveEvent {
            id,
            at,
            actor,
            scope,
            event,
        });
    }
    Ok(out)
}

fn run_event(event_id: i64, at: i64, kind: &'static str, payload: Value) -> RunEventProjection {
    RunEventProjection {
        event_id,
        at,
        kind,
        payload,
    }
}

fn record_earliest(
    map: &mut BTreeMap<String, RunEventProjection>,
    key: &str,
    event: RunEventProjection,
) {
    match map.get(key) {
        Some(existing) if existing.event_id <= event.event_id => {}
        _ => {
            map.insert(key.to_string(), event);
        }
    }
}

fn record_latest(
    map: &mut BTreeMap<String, RunEventProjection>,
    key: &str,
    event: RunEventProjection,
) {
    match map.get(key) {
        Some(existing) if existing.event_id >= event.event_id => {}
        _ => {
            map.insert(key.to_string(), event);
        }
    }
}

fn latest_final_event<'a>(
    completed: Option<&'a RunEventProjection>,
    failed: Option<&'a RunEventProjection>,
) -> Option<(&'static str, &'a RunEventProjection)> {
    match (completed, failed) {
        (Some(done), Some(fail)) if done.event_id > fail.event_id => Some(("completed", done)),
        (Some(_), Some(fail)) => Some(("failed", fail)),
        (Some(done), None) => Some(("completed", done)),
        (None, Some(fail)) => Some(("failed", fail)),
        (None, None) => None,
    }
}

fn is_spec_verdict_event(scope: &EventScope, actor: &ActorId) -> bool {
    matches!(scope, EventScope::Wave { .. }) && !matches!(actor, ActorId::KernelDispatcher)
}

fn verdict_from_event(event: &RunEventProjection) -> Option<RunVerdictProjection> {
    let (status, reason) = match event.kind {
        "task.completed" => {
            let result = event.payload.get("result")?;
            let status = result.get("status")?.as_str()?;
            (
                status,
                result
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        "task.failed" => (
            "rejected",
            event
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        _ => return None,
    };
    Some(RunVerdictProjection {
        status: status.to_string(),
        reason,
        at: event.at,
    })
}

pub(super) fn idempotency_key_from_payload(payload: &Value) -> Option<&str> {
    payload.get("idempotency_key").and_then(Value::as_str)
}

fn run_key_is_visible(key: &str) -> bool {
    if wave_fs_view::is_reserved_run_key(key) {
        // Deliberate VCS/live-view divergence; see `insert_run_entries`.
        tracing::error!(
            target: "wave_vcs",
            idempotency_key = %key,
            "runs projection: skipping idempotency_key that collides with reserved path"
        );
        false
    } else {
        true
    }
}

fn run_kind_from_card(card: &Card) -> Option<&'static str> {
    match card.kind.as_str() {
        "codex" => Some("codex"),
        "terminal" => Some("terminal"),
        _ => card
            .payload
            .get("role_request")
            .and_then(Value::as_str)
            .and_then(|kind| match kind {
                "codex" => Some("codex"),
                "terminal" => Some("terminal"),
                _ => None,
            }),
    }
}
