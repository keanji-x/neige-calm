//! Planner task rows are the durable maintenance requests; no header field is added.
use super::*;
use crate::auth::LegacyPrivateExecution;
use crate::harness::compact_outcome::{
    CompactionAnswer, CompactionRefusal, PlannerCompactionPhase,
};
use crate::model::{Task, TaskKind, TaskStatus};

#[derive(Clone)]
pub(super) enum Issuer {
    Human(HumanInputSource),
    Automation(LegacyPrivateExecution),
}
impl Issuer {
    pub(super) fn check_current(&self) -> Result<()> {
        match self {
            Self::Human(source) => source.current_principal().map(drop),
            Self::Automation(execution) => execution.current_owner().map(drop),
        }
    }

    pub(super) async fn authorize_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &crate::db::sqlite::conversation::CurrentInputCarrier,
    ) -> Result<()> {
        match self {
            Self::Human(source) => {
                crate::conversation_inputs::authorize_resource_tx(tx, &source.into(), carrier)
                    .await?;
            }
            Self::Automation(execution) => {
                let (binding, _) =
                    crate::harness::profile::input_carrier_binding_tx(tx, carrier).await?;
                if binding.profile != calm_types::harness::HarnessProfile::Planner {
                    return Err(CalmError::Forbidden(
                        "only the current Planner executes compact tasks".into(),
                    ));
                }
                let area: String = sqlx::query_scalar(
                    "SELECT t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?",
                )
                .bind(carrier.card_id())
                .fetch_one(&mut **tx)
                .await?;
                crate::organization_access::require_legacy_event_execution_tx(tx, &area, execution)
                    .await?;
            }
        }
        Ok(())
    }
}

async fn pending(inner: &Inner) -> Result<Option<Task>> {
    // Preserve the repository's scheduler order; the task row is the request.
    Ok(inner
        .repo
        .tasks_by_track(inner.track_id.as_str())
        .await?
        .into_iter()
        .find(|task| {
            task.kind == TaskKind::Compact
                && matches!(task.status, TaskStatus::Dispatched | TaskStatus::Running)
        }))
}

pub(in crate::harness::run_loop) async fn request(
    inner: &Inner,
    request_id: &str,
) -> Result<CompactionAnswer> {
    if inner.card_role_cache.get(&inner.card_id) != Some(crate::model::CardRole::Planner) {
        return Ok(CompactionAnswer::Refused(
            CompactionRefusal::ConversationUnavailable,
        ));
    }
    let current = canonical::read_current(inner).await?;
    if let Some(outcome) = current.snapshot.header.compact_outcome.as_ref() {
        if outcome.operation_id == request_id {
            return Ok(CompactionAnswer::Scheduled);
        }
        if outcome.status == CompactStatus::Started {
            return Ok(match inner.repo.task_get(&outcome.operation_id).await? {
                Some(task)
                    if task.kind == TaskKind::Compact
                        && task.track_id == inner.track_id.as_str() =>
                {
                    CompactionAnswer::AlreadyPending {
                        request_id: task.id,
                    }
                }
                Some(_) | None => {
                    CompactionAnswer::Refused(CompactionRefusal::CompactionInProgress)
                }
            });
        }
    }
    if let Some(task) = pending(inner).await? {
        if task.id != request_id {
            return Ok(CompactionAnswer::AlreadyPending {
                request_id: task.id,
            });
        }
        if task.status == TaskStatus::Dispatched
            && !inner.shutting_down.load(Ordering::SeqCst)
            && current.snapshot.header.wedged_reason.is_none()
            && current.snapshot.header.last_turn_id.is_some()
        {
            return Ok(CompactionAnswer::Scheduled);
        }
    }
    Ok(CompactionAnswer::Refused(
        CompactionRefusal::ConversationUnavailable,
    ))
}

pub(in crate::harness::run_loop) async fn tick(inner: &Arc<Inner>) -> Result<()> {
    if inner.card_role_cache.get(&inner.card_id) != Some(crate::model::CardRole::Planner) {
        return Ok(());
    }
    let current = canonical::read_current(inner).await?;
    if require_idle(&current.snapshot).is_err() {
        return Ok(());
    }
    let Some(task) = pending(inner)
        .await?
        .filter(|task| task.status == TaskStatus::Dispatched)
    else {
        return Ok(());
    };
    let Some(execution) = inner
        .inputs
        .canonical()
        .expect("canonical maintenance")
        .maintenance_execution()
    else {
        return Ok(());
    };
    // Admission leaves the row dispatched on any idle/fence failure. Start alone
    // consumes it, in the same transaction as the runtime frame CAS.
    start_with_issuer(inner, Issuer::Automation(execution), task.id).await
}

pub(in crate::harness::run_loop) async fn append_transition_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &TrackId,
    scope: &EventScope,
    previous: &calm_types::harness::snapshot::HarnessRuntimeHeader,
    next: &calm_types::harness::snapshot::HarnessRuntimeHeader,
    start: bool,
) -> Result<Vec<crate::event::BroadcastEnvelope>> {
    let Some(outcome) = next.compact_outcome.as_ref() else {
        return Ok(vec![]);
    };
    if !start
        && previous.compact_outcome.as_ref().is_none_or(|old| {
            old.operation_id != outcome.operation_id || old.status == outcome.status
        })
    {
        return Ok(vec![]);
    }
    let Some(task) = crate::db::sqlite::task_get_tx(tx, &outcome.operation_id).await? else {
        return if start {
            Err(CalmError::Conflict(
                "compaction task no longer exists".into(),
            ))
        } else {
            Ok(vec![])
        };
    };
    if task.kind != TaskKind::Compact || task.track_id != track.as_str() {
        return Err(CalmError::Conflict("compaction task owner mismatch".into()));
    }
    let phase = match outcome.status {
        CompactStatus::Started => PlannerCompactionPhase::Submitted {
            operation_id: outcome.operation_id.clone(),
        },
        CompactStatus::Completed
        | CompactStatus::Interrupted
        | CompactStatus::Failed
        | CompactStatus::Refused
        | CompactStatus::Unconfirmed => PlannerCompactionPhase::Settled {
            status: outcome.status.clone(),
            operation_id: outcome.operation_id.clone(),
        },
    };
    let mut events = super::super::super::self_compact::append_lifecycle(
        tx, track, &task.id, &phase, scope, start,
    )
    .await?;
    // A successful task CAS owns the one wake. Refreshes and terminal rows cannot
    // append another, even if a later boundary changes context validity.
    if !events.is_empty() {
        let wake = super::super::super::self_compact::prepare_request_wake(
            track,
            scope.clone(),
            &task.id,
            &outcome.status,
        );
        if let Some(wake) = super::super::super::self_compact::append_wake(tx, Some(wake)).await? {
            events.push(wake);
        }
    }
    Ok(events)
}
