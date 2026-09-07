//! Read-only admission from the original isolated Operation's retained stop identity.
use super::{journal, record::Admission};
use crate::dedicated_codex::StopState;
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskStatus};
use crate::operation::Tx;
use sha2::{Digest, Sha256};

fn denied() -> CalmError {
    CalmError::Conflict("predecessor isolated execution has no matching confirmed namespace stop; wait for its execution to stop before retrying".into())
}

/// Used by the shared predecessor fence at recovery admission and again before
/// successor claim/preparation. No live card/session or filesystem is authority.
pub(crate) async fn require_stopped_tx(tx: &mut Tx<'_>, task: &Task, op_id: &str) -> Result<()> {
    if task.status != TaskStatus::Failed || !super::selected(task).map_err(|_| denied())? {
        return Err(denied());
    }
    let valid_operation: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND kind='codex-isolated-worker' \
         AND idempotency_key=?2 AND phase='failed' AND spawn_artifacts_json IS NULL \
         AND compensation_state IS NULL)",
    )
    .bind(op_id)
    .bind(&task.id)
    .fetch_one(&mut **tx)
    .await?;
    if !valid_operation {
        return Err(denied());
    }
    confirmed_record_tx(tx, task, op_id).await.map(|_| ())
}

/// Typed original stop identity only; callers separately require their exact
/// task/Operation terminal outcome (failed for retry, done/succeeded for files).
pub(super) async fn confirmed_record_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    op_id: &str,
) -> Result<super::record::RunRecord> {
    // The typed private reader already binds Operation payload/output/target.
    // Do not expose parse errors or the private record through this capability.
    let record = journal::load_tx(tx, op_id).await.map_err(|_| denied())?;
    let session = record.session().map_err(|_| denied())?;
    let StopState::Quiesced(proof) = &session.stop else {
        return Err(denied());
    };
    let identity = &record.request.identity;
    let endpoint = &session.endpoint;
    let boundary = &endpoint.boundary;
    let launch = endpoint.launch_request();
    let request_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&record.request)?));
    if record.admission != Admission::Closed
        || record.track_id != task.track_id
        || identity.run_id != op_id
        || identity.attempt_id != task.id
        || identity.card_id.is_empty()
        || identity.session_id.is_empty()
        || task
            .worker_card_id
            .as_ref()
            .is_some_and(|card| card != &identity.card_id)
        || endpoint.version != 2
        || endpoint.request != record.request
        || launch.attempt_id != identity.attempt_id
        || launch.workspace != record.request.workspace
        || endpoint.home.run_id != identity.run_id
        || endpoint.home.request_digest != request_digest
        || boundary.run_id != identity.run_id
        || boundary.attempt_id != identity.attempt_id
        || proof.handle != *boundary
        || boundary.config_digest.len() != 64
        || !boundary
            .config_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || boundary.init.pid <= 0
        || boundary.init.start_time == 0
        || boundary.init.boot_id.is_empty()
        || boundary.init.namespace_inode == 0
        || proof.observed_at_ms == 0
        || !matches!(
            proof.method.as_str(),
            "prior_boot" | "init_absent" | "init_reaped" | "init_pid_reused"
        )
    {
        return Err(denied());
    }
    Ok(record)
}
