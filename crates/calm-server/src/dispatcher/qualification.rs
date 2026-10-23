//! Current task results carry their owner's notification declaration. Only the
//! explicit old-protocol reader consults historical task and delivery facts.
use super::*;
use crate::error::Result;
mod legacy;

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
    match event.task_notification() {
        Some(calm_types::event::PlannerNotification::Notify) => return Ok(false),
        Some(calm_types::event::PlannerNotification::Suppress) => return Ok(true),
        Some(calm_types::event::PlannerNotification::Legacy) => {}
        None => return Ok(false),
    }
    legacy::deferred_self_report(repo, event, reads).await
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
