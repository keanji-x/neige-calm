//! Event qualification owns the required task/delivery reads. Historical audit
//! push retains its published fail-open behavior; durable acceptance propagates
//! missing read results so its ordered prefix remains replayable.
use super::*;
use crate::error::Result;

#[derive(Clone, Copy)]
enum FactReads {
    HistoricalAudit,
    DurableAcceptance,
}
impl FactReads {
    fn required(self) -> bool {
        matches!(self, Self::DurableAcceptance)
    }
}

pub(super) async fn qualified_observation(
    repo: &dyn Repo,
    track: &TrackId,
    envelope: &crate::db::TrackEvent,
) -> Result<Option<HarnessObservation>> {
    let event = &envelope.event;
    let role = match event {
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            repo.card_role_get(card_id.as_str()).await?
        }
        _ => None,
    };
    if !event_warrants_planner_push_with_role(event, &envelope.actor, |_| role)
        || deferred_self_report(repo, event, FactReads::DurableAcceptance).await?
        || stale_worker_stop_hook(repo, event, FactReads::DurableAcceptance).await?
    {
        return Ok(None);
    }
    resolve_event_observation(repo, track, event, true).await
}

/// A later delivery/gate event carries the wake for a deferred worker self-report.
/// The historical caller preserves delivery-error -> consult gate and task-error
/// -> push. The durable prefix must know these required facts before accepting.
pub(crate) async fn is_deferred_self_report(repo: &dyn Repo, event: &Event) -> bool {
    deferred_self_report(repo, event, FactReads::HistoricalAudit)
        .await
        .unwrap_or(false)
}

async fn deferred_self_report(repo: &dyn Repo, event: &Event, reads: FactReads) -> Result<bool> {
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

/// Historical worker hooks push on missing task rows or read failure. Required
/// durable qualification distinguishes a known missing row from an unavailable read.
pub(crate) async fn is_stale_worker_stop_hook(repo: &dyn Repo, event: &Event) -> bool {
    stale_worker_stop_hook(repo, event, FactReads::HistoricalAudit)
        .await
        .unwrap_or(false)
}

async fn stale_worker_stop_hook(repo: &dyn Repo, event: &Event, reads: FactReads) -> Result<bool> {
    let card_id = match event {
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => card_id,
        _ => return Ok(false),
    };
    match repo.task_for_worker_card(card_id.as_str()).await {
        Ok(Some(task)) => Ok(!matches!(
            task.status,
            crate::model::TaskStatus::Dispatched | crate::model::TaskStatus::Running
        )),
        Ok(None) => Ok(false),
        Err(error) => {
            if !reads.required() {
                tracing::warn!(%card_id, %error,
                    "dispatcher push: stale-worker-stop lookup failed; pushing stop hook (fail-open)");
            }
            Err(error.into())
        }
    }
}
