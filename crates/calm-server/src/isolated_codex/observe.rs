//! Lifetime observer of one parked Operation, reusing its lease and task writers.
use super::{adapter::IsolatedCodexAdapter, config::provider_error, record::RunRecord};
use crate::db::{write_in_tx_typed, write_with_actor_events_typed};
use crate::dedicated_codex::{RequestPhase, Session};
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskStatus};
use crate::operation::{
    Operation, ParkedObserver, ParkedOutcome, ParkedRecovery, RecoveryMode, SpawnCtx,
};
use calm_worker_runtime::BoundaryState;
use std::time::Duration;

fn terminal(task: &Task) -> bool {
    matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled
    )
}

async fn fail(
    adapter: &IsolatedCodexAdapter,
    op: &Operation,
    ctx: &SpawnCtx,
    reason: &str,
) -> Result<()> {
    let op = op.clone();
    let reason = reason.to_string();
    write_with_actor_events_typed(
        adapter.repo.as_ref(),
        None,
        &ctx.events,
        &adapter.write,
        move |tx| {
            Box::pin(async move {
                super::journal::require_owner_tx(tx, &op).await?;
                let record = super::journal::load_tx(tx, &op.id).await?;
                let Some(task) =
                    crate::db::sqlite::task_get_tx(tx, &record.request.identity.attempt_id).await?
                else {
                    return Ok(((), vec![]));
                };
                if terminal(&task) {
                    return Ok(((), vec![]));
                }
                let track =
                    crate::track_lifecycle::track_get_tx(tx, &task.track_id.clone().into()).await?;
                let events = crate::scheduler::fail_worker_task_tx(
                    tx,
                    &task,
                    &track,
                    "worker-exit",
                    &reason,
                )
                .await?;
                Ok(((), events))
            })
        },
    )
    .await
    .map(|_| ())
}

pub(crate) async fn stop(
    adapter: &IsolatedCodexAdapter,
    op: &Operation,
    ctx: &SpawnCtx,
) -> Result<()> {
    let owned = op.clone();
    write_in_tx_typed(adapter.repo.as_ref(), move |tx| {
        Box::pin(async move { super::journal::close_tx(tx, &owned).await.map(|_| ()) })
    })
    .await?;
    // Reconcile an interrupted dormant prepare by its original immutable request.
    // This cannot authorize provider start and never substitutes another run ID.
    let record = adapter.prepare_endpoint(op).await?;
    let mut session = record.session()?.clone();
    let checkpoint = adapter.checkpoint(op, ctx)?;
    let state = adapter
        .backend()?
        .controller
        .stop(&mut session, &checkpoint, Duration::from_secs(5))
        .await
        .map_err(provider_error)?;
    if !matches!(state, BoundaryState::Quiesced(_)) {
        return Err(CalmError::Conflict(
            "isolated runtime stop is unproven; retaining owned execution".into(),
        ));
    }
    let owned = op.clone();
    write_in_tx_typed(adapter.repo.as_ref(), move |tx| {
        Box::pin(async move {
            super::journal::require_owner_tx(tx, &owned).await?;
            let record = super::journal::load_tx(tx, &owned.id).await?;
            if let Some(session) = crate::db::sqlite::session_projection_by_id_tx(
                tx,
                &record.request.identity.session_id,
            )
            .await?
                && !session.status.is_terminal()
            {
                crate::db::sqlite::session_set_status_tx(
                    tx,
                    &session.id,
                    crate::session_projection_repo::WorkerSessionState::Exited,
                )
                .await?;
            }
            Ok(())
        })
    })
    .await
}

async fn task(adapter: &IsolatedCodexAdapter, record: &RunRecord) -> Result<Option<Task>> {
    let id = record.request.identity.attempt_id.clone();
    write_in_tx_typed(adapter.repo.as_ref(), move |tx| {
        Box::pin(async move { Ok(crate::db::sqlite::task_get_tx(tx, &id).await?) })
    })
    .await
}

pub(crate) async fn reconcile(
    adapter: &IsolatedCodexAdapter,
    op: &Operation,
    mode: RecoveryMode,
    ctx: &SpawnCtx,
) -> Result<ParkedRecovery> {
    let record = adapter.record(op).await?;
    let mut current = task(adapter, &record).await?;
    if current.as_ref().is_some_and(|task| !terminal(task)) {
        let reason = if mode == RecoveryMode::PastDeadline {
            Some("Isolated Codex task deadline exceeded.".to_string())
        } else if current
            .as_ref()
            .is_some_and(|task| task.context_stale_at_ms.is_some())
        {
            Some("Task execution was withdrawn or its frozen requirements changed.".to_string())
        } else if !matches!(
            adapter
                .backend()?
                .controller
                .probe(&record.session()?.endpoint)
                .await
                .map_err(provider_error)?,
            BoundaryState::Running
        ) {
            Some("Isolated Codex runtime ended without a task report.".to_string())
        } else {
            None
        };
        if let Some(reason) = reason {
            fail(adapter, op, ctx, &reason).await?;
            current = task(adapter, &record).await?;
        }
    }
    if current.as_ref().is_none_or(terminal) {
        stop(adapter, op, ctx).await?;
        let outcome = match current {
            Some(task) if task.status == TaskStatus::Done => ParkedOutcome::Succeeded {
                result: op
                    .tx_output
                    .as_ref()
                    .map(|o| o.result.clone())
                    .unwrap_or_default(),
            },
            task => ParkedOutcome::Failed {
                last_error: task
                    .and_then(|t| t.status_detail)
                    .unwrap_or_else(|| "Isolated task canceled or removed.".into()),
                last_error_class: Some("isolated-execution".into()),
            },
        };
        return Ok(ParkedRecovery::Complete(outcome));
    }
    let backend = adapter.backend()?;
    if !backend.observers.lock().await.contains(&op.id) {
        let checkpoint = adapter.checkpoint(op, ctx)?;
        let attached = async {
            if !matches!(record.session()?.phase, RequestPhase::TurnActive { .. }) {
                return Err(CalmError::Conflict(
                    "isolated turn acknowledgement is uncertain; refusing to repeat it".into(),
                ));
            }
            let mut session = backend
                .controller
                .connect(record.session()?.clone(), &checkpoint)
                .await
                .map_err(provider_error)?;
            session
                .reconcile(&checkpoint)
                .await
                .map_err(provider_error)?;
            Ok(session)
        }
        .await;
        match attached {
            Ok(session) => {
                tokio::spawn(observer(adapter.clone(), op.clone(), ctx.clone(), session));
            }
            Err(error) => {
                fail(
                    adapter,
                    op,
                    ctx,
                    &format!("Could not reconcile the original isolated endpoint: {error}"),
                )
                .await?;
                stop(adapter, op, ctx).await?;
                return Ok(ParkedRecovery::Complete(ParkedOutcome::Failed {
                    last_error: error.to_string(),
                    last_error_class: Some("isolated-reconcile".into()),
                }));
            }
        }
    }
    Ok(ParkedRecovery::LeaveParked)
}

pub(crate) fn observer(
    adapter: IsolatedCodexAdapter,
    op: Operation,
    ctx: SpawnCtx,
    mut session: Session,
) -> ParkedObserver {
    Box::pin(async move {
        let Ok(backend) = adapter.backend().cloned() else {
            return;
        };
        if !backend.observers.lock().await.insert(op.id.clone()) {
            return;
        }
        let mut notifications = match session.take_notifications() {
            Ok(stream) => stream,
            Err(_) => {
                backend.observers.lock().await.remove(&op.id);
                return;
            }
        };
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut failure = None;
        loop {
            tokio::select! {
                notification=notifications.recv(),if failure.is_none()=>{
                    match notification {
                        Some(crate::codex_appserver::Notification::TurnCompleted{thread_id,turn,..})=>{
                            if matches!(&session.record().phase,RequestPhase::TurnActive{thread_id:expected,turn_id,..}
                                if expected==&thread_id && turn.get("id").and_then(serde_json::Value::as_str)==Some(turn_id)) {
                                failure=Some("Codex turn finished without an authorized task report.".to_string());
                            }
                        },
                        None=>failure=Some("Isolated Codex control connection closed without a task report.".into()),
                        _=>continue,
                    }
                },
                _=interval.tick()=>{},
            }
            let claimed = match ctx.operation_repo.claim_parked(&op.id).await {
                Ok(Some(op)) => op,
                Ok(None) => {
                    if ctx
                        .operation_repo
                        .operation_result(&op.id)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        break;
                    }
                    continue;
                }
                Err(_) => continue,
            };
            if let Some(reason) = &failure
                && let Err(error) = fail(&adapter, &claimed, &ctx, reason).await
            {
                tracing::warn!(operation_id=%op.id,%error,"isolated failure observation awaits reconciliation");
            }
            let mode = if claimed
                .parked_deadline_ms
                .is_some_and(|deadline| crate::model::now_ms() > deadline)
            {
                RecoveryMode::PastDeadline
            } else {
                RecoveryMode::PreDeadlineProbe
            };
            if let Err(error) =
                crate::operation::owned_parked::reconcile(&adapter, &claimed, mode, &ctx).await
            {
                tracing::warn!(operation_id=%op.id,%error,"isolated stop remains unresolved");
            }
            if ctx
                .operation_repo
                .operation_result(&op.id)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                break;
            }
        }
        backend.observers.lock().await.remove(&op.id);
        drop(session);
    })
}
