//! Read-only admission from the original isolated Operation's retained stop identity.
use super::{journal, record::Admission};
use crate::dedicated_codex::StopState;
use crate::model::{Task, TaskStatus};
use crate::operation::Tx;
use crate::task_recovery::{
    AdmissionError, RecoveryRefusal, RecoveryRefusalCode, RefusalSite, SupportedContinuation,
};
use sha2::{Digest, Sha256};

fn denied(
    site: RefusalSite,
    continuation: SupportedContinuation,
    reason: String,
) -> AdmissionError {
    RecoveryRefusal::conflict(
        site,
        RecoveryRefusalCode::PredecessorNotQuiescent,
        continuation,
        reason,
    )
    .into()
}

fn permanent(site: RefusalSite, reason: String) -> AdmissionError {
    denied(site, SupportedContinuation::None, reason)
}

/// No live card/session or filesystem is authority.
pub(crate) async fn require_stopped_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    op_id: &str,
) -> Result<(), AdmissionError> {
    if task.status != TaskStatus::Failed || !super::selected(task).unwrap_or(false) {
        return Err(permanent(
            RefusalSite::IsolatedRouteMismatch,
            "predecessor isolated operation does not belong to a failed isolated-route execution; \
             its namespace stop cannot fence this key, so same-key recovery is permanently \
             unavailable"
                .into(),
        ));
    }
    let operation: Option<(String, String, bool, bool)> = sqlx::query_as(
        "SELECT kind, phase, spawn_artifacts_json IS NOT NULL, compensation_state IS NOT NULL \
         FROM operations WHERE id=?1 AND idempotency_key=?2",
    )
    .bind(op_id)
    .bind(&task.id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((_, phase, spawn_artifacts, compensation)) =
        operation.filter(|(kind, ..)| kind == super::OPERATION_KIND)
    else {
        return Err(permanent(
            RefusalSite::IsolatedOperationNotThisExecution,
            "predecessor isolated operation named for settlement is not this execution's keyed \
             isolated operation; its namespace stop cannot fence this key, so same-key recovery \
             is permanently unavailable"
                .into(),
        ));
    };
    if spawn_artifacts || compensation {
        return Err(permanent(
            RefusalSite::IsolatedCompensationRecorded,
            "predecessor isolated execution recorded spawn artifacts or compensation state, so \
             its namespace stop is not attributable; same-key recovery is permanently \
             unavailable and no settlement briefing re-opens it"
                .into(),
        ));
    }
    if phase != "failed" {
        if matches!(phase.as_str(), "succeeded" | "stuck") {
            return Err(permanent(
                RefusalSite::IsolatedOperationTerminalWithoutFailure,
                format!(
                    "predecessor isolated operation ended in phase {phase} rather than failed; \
                     the failed execution has no matching stop proof, so same-key recovery is \
                     permanently unavailable"
                ),
            ));
        }
        // The Controller may already have checkpointed `Quiesced` before the separate
        // parked-completion transaction; this branch tests the phase alone.
        return Err(denied(
            RefusalSite::IsolatedStopPending,
            SupportedContinuation::WaitForSettlement,
            format!(
                "predecessor isolated operation is in phase {phase}, not failed; the settlement \
                 that records its stop has not completed, so recovery re-opens once the kernel \
                 settles it and delivers the settlement briefing"
            ),
        ));
    }
    confirmed_record_tx(tx, task, op_id).await.map(|_| ())
}

/// Typed original stop identity only; callers separately require their exact
/// task/Operation terminal outcome (failed for retry, done/succeeded for files).
pub(super) async fn confirmed_record_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    op_id: &str,
) -> Result<super::record::RunRecord, AdmissionError> {
    // Do not expose parse errors or the private record through this capability.
    let unreadable = || {
        permanent(
            RefusalSite::IsolatedRecordUnreadable,
            "predecessor isolated operation's journal cannot be read as a prepared run, so no \
             namespace stop proof exists for it; same-key recovery is permanently unavailable"
                .into(),
        )
    };
    let record = journal::load_tx(tx, op_id)
        .await
        .map_err(|_| unreadable())?;
    let session = record.session().map_err(|_| unreadable())?;
    let StopState::Quiesced(proof) = &session.stop else {
        return Err(permanent(
            RefusalSite::IsolatedStopUnconfirmed,
            "predecessor isolated execution has no recorded namespace quiescence proof although \
             its operation is terminal, and the kernel will not record one; same-key recovery is \
             permanently unavailable"
                .into(),
        ));
    };
    let identity = &record.request.identity;
    let endpoint = &session.endpoint;
    let boundary = &endpoint.boundary;
    let launch = endpoint.launch_request();
    let request_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&record.request)?));
    let admission_open = record.admission != Admission::Closed;
    if admission_open
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
        let unmet = if admission_open {
            format!(
                "has admission state {}, not closed",
                record.admission.as_str()
            )
        } else {
            "has an identity chain or stop proof that does not validate for this execution".into()
        };
        return Err(permanent(
            RefusalSite::IsolatedStopIdentityMismatch,
            format!(
                "predecessor isolated execution's recorded run {unmet}; same-key recovery is \
                 permanently unavailable"
            ),
        ));
    }
    Ok(record)
}
