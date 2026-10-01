//! Read-only task/report projections; reports remain in their original event rows.
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::model::Card;
use calm_types::worker_presentation::{
    WorkerSnapshot, WorkerSnapshotOutcome, WorkerSnapshotReport,
};
use serde_json::Value;
use std::collections::HashMap;

pub(super) async fn project(repo: &dyn RouteRepo, cards: &mut [Card]) -> Result<()> {
    let ids: Vec<String> = cards
        .iter()
        .filter(|card| card.kind == "codex")
        .map(|card| card.id.to_string())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let snapshots = write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let mut snapshots = HashMap::new();
        for card in ids {
            let row: Option<(String,String,String,String)> = sqlx::query_as(
                "SELECT task.id,task.goal,task.status,task.track_id FROM tasks task \
                 JOIN operations operation ON operation.idempotency_key=task.id \
                 JOIN workspace_leases lease ON lease.lease_owner=operation.id AND lease.holder_kind='task' \
                 WHERE lease.card_id=?1 AND operation.kind='codex-worker' ORDER BY task.created_at_ms DESC LIMIT 1"
            ).bind(&card).fetch_optional(&mut **tx).await?;
            let Some((task_id,goal,status,track)) = row else { continue; };
            let report: Option<(String,String)> = sqlx::query_as(
                "SELECT kind,payload FROM events WHERE scope_track=?1 \
                 AND kind IN ('task.completed','task.failed') AND json_valid(payload) \
                 AND json_extract(payload,'$.idempotency_key')=?2 ORDER BY id DESC LIMIT 1"
            ).bind(&track).bind(&task_id).fetch_optional(&mut **tx).await?;
            let report = match report {
                Some((kind,payload)) => {
                    let original: Value = serde_json::from_str(&payload)?;
                    match Event::from_kind_and_payload(&kind,original.clone())? {
                        Event::TaskCompleted { result,.. } => WorkerSnapshotReport::Reported {
                            outcome: WorkerSnapshotOutcome::Completed, result,
                        },
                        Event::TaskFailed { .. } => WorkerSnapshotReport::Reported {
                            outcome: WorkerSnapshotOutcome::Failed, result: original,
                        },
                        _ => return Err(CalmError::Internal("unexpected task report kind".into())),
                    }
                },
                None => WorkerSnapshotReport::Pending,
            };
            let status = status.try_into().map_err(CalmError::Internal)?;
            snapshots.insert(card, WorkerSnapshot { task_id,goal,status,report });
        }
        Ok(snapshots)
    })).await?;
    for card in cards {
        if let Some(snapshot) = snapshots.get(card.id.as_str())
            && let Some(payload) = card.payload.as_object_mut()
        {
            payload.insert("worker_snapshot".into(), serde_json::to_value(snapshot)?);
        }
    }
    Ok(())
}
