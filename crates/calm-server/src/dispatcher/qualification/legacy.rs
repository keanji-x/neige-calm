//! Compatibility for task reports persisted before owner-declared notification.
use super::*;

/// Only immutable pre-v31 reports lack the owner's declaration.
pub(super) async fn deferred_self_report(
    repo: &dyn Repo,
    event: &Event,
    reads: FactReads,
) -> Result<bool> {
    let (idempotency_key, is_failure) = match event {
        Event::TaskCompleted {
            idempotency_key, ..
        } => (idempotency_key, false),
        Event::TaskFailed {
            idempotency_key, ..
        } => (idempotency_key, true),
        _ => return Ok(false),
    };
    if !is_failure && let Some(pool) = repo.sqlite_db() {
        match crate::git_candidate::delivery::attempt_has_delivery(
            pool.read_pool(),
            idempotency_key,
        )
        .await
        {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                if reads.required() {
                    return Err(error);
                }
                tracing::warn!(%idempotency_key, %error, "dispatcher push: delivery-row lookup failed; consulting the gate rule (fail-open)");
            }
        }
    }
    match repo.task_get(idempotency_key).await {
        Ok(Some(task)) => {
            if task.kind == crate::model::TaskKind::Compact
                && ((!is_failure && task.status == crate::model::TaskStatus::Done)
                    || (is_failure
                        && task
                            .status_detail
                            .as_deref()
                            .map(crate::db::sqlite::status_detail_class)
                            == Some("compaction-incomplete")))
            {
                return Ok(true);
            }
            if task.gate_json.is_none() {
                return Ok(false);
            }
            if !is_failure {
                return Ok(true);
            }
            let failure_landed_pre_gate = task.status == crate::model::TaskStatus::Failed
                && matches!(
                    task.status_detail
                        .as_deref()
                        .map(crate::db::sqlite::status_detail_class),
                    Some("worker-reported")
                        | Some("spawn-failed")
                        | Some("worker-timeout")
                        | Some("child-track-incomplete")
                        | Some("child-track-deleted")
                        | Some(crate::scheduler::WORKER_TURN_ENDED)
                );
            Ok(!failure_landed_pre_gate)
        }
        Ok(None) => Ok(false),
        Err(error) => {
            if !reads.required() {
                tracing::warn!(%idempotency_key, %error,
                    "dispatcher push: gated-self-report lookup failed; pushing self-report (fail-open)");
            }
            Err(error.into())
        }
    }
}
