//! Full pre-effect evidence for a dispatched checkout worker that has yielded.
use crate::{
    error::Result,
    model::{Task, TaskStatus, now_ms},
};
use calm_types::task_execution::PendingBlocker;
use sqlx::SqliteConnection;

/// A valid blocker alone grants nothing. The operation must belong to this task,
/// still be unprepared, and have no live claimant. Leases and deliveries remain
/// independent occupancy terms even when this returns a blocker.
pub async fn pending_checkout_blocker(
    conn: &mut SqliteConnection,
    task: &Task,
) -> Result<Option<(String, PendingBlocker)>> {
    if task.status != TaskStatus::Dispatched || !task.runs_in_track_checkout() {
        return Ok(None);
    }
    let Some(kind) = task.kind.worker_operation_kind() else {
        return Ok(None);
    };
    #[derive(sqlx::FromRow)]
    struct Facts {
        id: String,
        payload_json: String,
        phase_detail_json: Option<String>,
        lease_owner: Option<String>,
        lease_until_ms: Option<i64>,
    }
    let row: Option<Facts> = sqlx::query_as(
        "SELECT id,payload_json,phase_detail_json,lease_owner,lease_until_ms \
         FROM operations WHERE kind=?1 AND idempotency_key=?2 AND phase='pending' \
           AND tx_output_json IS NULL AND spawn_artifacts_json IS NULL \
           AND completed_at_ms IS NULL",
    )
    .bind(kind)
    .bind(&task.id)
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    if row.lease_owner.is_some()
        && row
            .lease_until_ms
            .is_none_or(|deadline| deadline >= now_ms())
    {
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Binding {
        track_id: String,
        idempotency_key: String,
    }
    let Ok(binding) = serde_json::from_str::<Binding>(&row.payload_json) else {
        return Ok(None);
    };
    if binding.track_id != task.track_id || binding.idempotency_key != task.id {
        return Ok(None);
    }
    let Some(detail) = row.phase_detail_json else {
        return Ok(None);
    };
    let Ok(blocker) = serde_json::from_str::<PendingBlocker>(&detail) else {
        // Invalid evidence cannot make a writer disappear from occupancy.
        return Ok(None);
    };
    Ok(Some((row.id, blocker)))
}
