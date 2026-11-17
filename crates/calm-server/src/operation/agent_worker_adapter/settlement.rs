//! Task termination and workspace release share one writer transaction.
use super::super::workspace_lease::{
    ReleaseDelivery,
    release::{EndedAttempt, release_ended_attempt_lease_for_card_tx},
};
use super::*;
use crate::db::sqlite::{TaskReporter, session_complete_tx, task_fail_from_worker_tx};
use crate::session_projection_repo::WorkerSessionState;

pub(super) async fn ended(
    adapter: &AgentWorkerAdapter,
    ctx: &SpawnCtx,
    attempt: &str,
    card: &str,
    worker: &str,
) -> Result<()> {
    settle(
        ctx.repo.as_ref(),
        &ctx.events,
        &crate::state::WriteContext::new(adapter.roles.clone(), adapter.areas.clone()),
        attempt,
        card,
        worker,
    )
    .await
}

pub(crate) async fn settle(
    repo: &dyn crate::db::RepoEventWrite,
    bus: &crate::event::EventBus,
    write: &crate::state::WriteContext,
    attempt: &str,
    card: &str,
    worker: &str,
) -> Result<()> {
    let attempt = attempt.to_owned();
    let card = card.to_owned();
    let worker = worker.to_owned();
    let scope_write = write.clone();
    write_with_actor_events_typed(repo, None, bus, write, move |tx| {
        Box::pin(async move {
            let events = settle_tx(tx, &scope_write, attempt, card, worker).await?;
            Ok(((), events))
        })
    })
    .await
    .map(|_| ())
}

async fn settle_tx(
    tx: &mut Tx<'_>,
    scope_write: &crate::state::WriteContext,
    attempt: String,
    card: String,
    worker: String,
) -> Result<
    Vec<(
        crate::ids::ActorId,
        crate::event::EventScope,
        crate::event::Event,
    )>,
> {
    let task = task_get_tx(tx, &attempt)
        .await?
        .ok_or_else(|| CalmError::NotFound("worker attempt".into()))?;
    if task.worker_card_id.as_deref() != Some(&card) {
        return Ok(vec![]);
    }
    let mut events = Vec::new();
    if task.status == crate::model::TaskStatus::Running
        && task.worker_card_id.as_deref() == Some(&card)
    {
        let rows = task_fail_from_worker_tx(
            tx,
            &attempt,
            &task.track_id,
            TaskReporter::Card {
                card_id: &card,
                owns_key: false,
            },
            crate::scheduler::WORKER_TURN_ENDED,
            now_ms(),
        )
        .await?;
        if rows == 1 {
            events.push((
                crate::ids::ActorId::KernelDispatcher,
                crate::event::EventScope::Track {
                    track: task.track_id.clone().into(),
                    area: scope_write
                        .verify_area(&task.track_id.clone().into())
                        .ok_or_else(|| CalmError::NotFound("worker area".into()))?,
                },
                crate::event::Event::TaskFailed {
                    notification: calm_types::event::PlannerNotification::Notify,
                    idempotency_key: attempt.clone(),
                    reason: "worker turn ended without a task report".into(),
                    details: None,
                    agent_message: None,
                },
            ));
        }
    }
    if let Some(task) = task_get_tx(tx, &attempt).await?
        && task.status.is_terminal()
    {
        events.extend(
            release_ended_attempt_lease_for_card_tx(
                tx,
                &card,
                EndedAttempt::Exactly(&attempt),
                ReleaseDelivery::CommitAsTaskEnded,
            )
            .await?,
        );
    }
    if let Some(session) = crate::db::sqlite::session_projection_by_id_tx(tx, &worker).await?
        && session.card_id == card
    {
        if let Some(mut state) = session.handle_state_json
            && let Some(object) = state.as_object_mut()
        {
            object.remove("timeout_cleanup");
            object.insert(
                "phase".into(),
                serde_json::to_value(crate::harness::HarnessPhaseTag::TurnCompleted)?,
            );
            crate::db::sqlite::session_set_handle_state_of_any_runtime_tx(
                tx,
                &worker,
                Some(state),
                now_ms(),
            )
            .await?;
        }
        crate::db::sqlite::session_set_active_turn_tx(tx, &worker, None).await?;
        if session.status.is_active_authority() {
            session_complete_tx(tx, &worker, WorkerSessionState::Exited).await?;
        }
    }
    Ok(events)
}

pub(super) async fn compensate(
    adapter: &AgentWorkerAdapter,
    output: &TxOutput,
    op: &Operation,
    ctx: &SpawnCtx,
) -> Result<()> {
    let card = output.output_string("card_id", "agent worker cleanup")?;
    let worker = output.output_string("worker_session_id", "agent worker cleanup")?;
    adapter.harness.stop_task_worker(&worker).await?;
    if let Some(runtime) = ctx.repo.session_projection_by_id(&worker).await?
        && runtime.agent_provider == Some(AgentProvider::Codex)
        && let Some(thread) = runtime.thread_id
    {
        let daemon = adapter.backends.for_session(&worker).await?;
        if let provider::control::NativeInterruptOutcome::Unconfirmed { error } =
            daemon.interrupt_active_outcome(&thread).await
        {
            return Err(error);
        }
    }
    // Stop managed ACP execution before retiring its storage or releasing writer ownership.
    adapter
        .wiring
        .acp
        .provider_execution
        .stop_session(
            &ctx.operation_repo.sqlite_db(),
            adapter.wiring.acp.execution.engine(),
            &worker,
            crate::provider_execution::StopReason::SetupFailed,
            std::time::Duration::from_secs(10),
        )
        .await?;
    let card = card.clone();
    let roles = adapter.roles.clone();
    let attempt = op
        .idempotency_key
        .clone()
        .ok_or_else(|| CalmError::Conflict("worker compensation has no attempt".into()))?;
    let write = crate::state::WriteContext::new(adapter.roles.clone(), adapter.areas.clone());

    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            if let Some(task) = task_get_tx(tx, &attempt).await?
                && task.worker_card_id.as_deref() == Some(card.as_str())
                && (task.status.is_terminal() || task.status == crate::model::TaskStatus::Verifying)
            {
                // A committed business outcome takes precedence over startup compensation.
                let events = settle_tx(tx, &write, attempt, card, worker).await?;
                return super::super::workspace_lease::append_workspace_events_tx(tx, events).await;
            }
            let existing = match crate::db::sqlite::card_for_update_tx(tx, &card).await {
                Ok(card) => Some(card),
                Err(error) if error.is_not_found() => None,
                Err(error) => return Err(error.into()),
            };
            let mut events =
                super::super::workspace_lease::release::release_workspace_lease_for_card_tx(
                    tx,
                    &card,
                    ReleaseDelivery::Commit(
                        crate::git_candidate::delivery::AttemptOutcome::SpawnFailed,
                    ),
                )
                .await?;
            if let Some(existing) = existing {
                let scope = crate::routes::cards::card_scope_tx(
                    tx,
                    existing.id.clone(),
                    existing.track_id.clone(),
                )
                .await?;
                crate::db::sqlite::card_delete_tx(tx, &card, &roles).await?;
                events.push((
                    crate::ids::ActorId::KernelDispatcher,
                    scope,
                    crate::event::Event::CardDeleted {
                        id: existing.id,
                        track_id: existing.track_id,
                    },
                ));
            }
            let envelopes =
                super::super::workspace_lease::append_workspace_events_tx(tx, events).await?;
            Ok(envelopes)
        })
    })
    .await?
    .into_iter()
    .for_each(|e| ctx.events.emit_envelope(e));
    Ok(())
}

/// Both boot and steady reconciliation retry only sessions with no live runner.
pub(super) async fn recover(adapter: &AgentWorkerAdapter, ctx: &SpawnCtx) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let rows:Vec<(String,String,String)>=sqlx::query_as(
        "SELECT t.id,c.id,ws.id FROM tasks t JOIN cards c ON t.worker_card_id=c.id JOIN worker_sessions ws ON c.session_id=ws.id
         WHERE json_extract(c.payload,'$.harness_profile')='task_worker' AND ws.contract='executor'
         AND ws.terminal_run_id IS NULL AND (ws.state IN ('starting','running','idle','turn_pending')
           OR (ws.state='failed' AND EXISTS(SELECT 1 FROM workspace_leases l WHERE l.card_id=c.id AND l.state IN ('held','releasing'))))")
        .fetch_all(pool.read_pool()).await?;
    let mut first_error = None;
    for (attempt, card, worker) in rows {
        if adapter.harness.has_task_worker(&worker) {
            continue;
        }
        let recovered = async {
            let runtime = ctx
                .repo
                .session_projection_by_id(&worker)
                .await?
                .ok_or_else(|| {
                    CalmError::Conflict("worker session disappeared during recovery".into())
                })?;
            if runtime.agent_provider == Some(AgentProvider::Codex) {
                let daemon = adapter.backends.for_session(&worker).await?;
                crate::harness::backend::factory::PlannerFactory::stop_timed_out_worker(
                    &runtime,
                    Some(daemon.as_ref()),
                )
                .await?;
            } else {
                adapter
                    .wiring
                    .acp
                    .provider_execution
                    .stop_session(
                        &pool,
                        adapter.wiring.acp.execution.engine(),
                        &worker,
                        crate::provider_execution::StopReason::DriverLost,
                        std::time::Duration::from_secs(10),
                    )
                    .await?;
            }
            ended(adapter, ctx, &attempt, &card, &worker).await
        }
        .await;
        if let Err(error) = recovered {
            tracing::warn!(%error,%worker,"worker session recovery deferred");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
