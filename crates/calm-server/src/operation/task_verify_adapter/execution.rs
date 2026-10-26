//! Task verification consumes the gate-owned execution and its durable release/decision record.
use super::*;
use crate::operation::gate_execution::{self as execution, Decision, Release, StopReason};
use workspace_runtime::execution::{ExecutionState, Settlement};

impl TaskVerifyAdapter {
    pub(super) async fn spawn_execution(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-verify-post-prepare");
        let frozen = FrozenVerify::from_output(output)?;
        if frozen.target.spawn_is_noop() {
            return Ok(SpawnOutcome::Ready(super::super::SpawnHandle::NoOp));
        }
        let redrive = output.data.get(execution::KEY).is_some();
        if redrive {
            execution::record(op)?;
        } else {
            if op.spawn_artifacts.is_some() {
                return Err(legacy_error(op));
            }
            execution::engine(ctx)?;
            // No killing or PID fallback: an earlier execution must have settled before
            // this consumer can acquire a new execution for the same task.
            let pool = ctx.operation_repo.sqlite_db();
            let mut tx = begin_immediate_tx(&pool).await?;
            require_prior_settled(&mut tx, &frozen.task_id, frozen.attempt).await?;
            tx.commit().await?;
            super::super::admit_task_side_effect(ctx.repo.as_ref(), &frozen.task_id).await?;
            let paths = self.paths(&frozen.task_id, frozen.attempt);
            paths.unlink_stale(&self.gate_logs_dir).await?;
            execution::admit(
                op,
                ctx,
                &frozen.cwd,
                &frozen.track_id,
                GateFiles {
                    script: &paths.script,
                    log: &paths.log,
                    exit: &paths.exit,
                    step: &paths.step,
                },
                crate::operation::gate_process::WrapperSteps::Logged(&frozen.gate.steps),
                &gate_attempt_key(&frozen.task_id, frozen.attempt),
            )
            .await?;
        }
        let (adapter, ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
        Ok(SpawnOutcome::Parked {
            deadline_ms: frozen.gate.parked_deadline_ms(now_ms()),
            observer: Box::pin(async move { adapter.observe(ctx, id, redrive, false).await }),
        })
    }

    pub(super) async fn recover_execution(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        if super::super::required_output(op)?
            .data
            .get(execution::KEY)
            .is_none()
        {
            return Err(legacy_error(op));
        }
        let record = execution::record(op)?;
        let frozen = FrozenVerify::from_output(super::super::required_output(op)?)?;
        let settled =
            crate::operation::owned_execution::settlement(ctx, &record.execution_id).await?;
        if settled.is_none() {
            if matches!(mode, RecoveryMode::Boot) {
                if record.decision == Decision::Observing {
                    match record.release {
                        Release::Held => {
                            execution::stop(ctx, op, StopReason::KernelRestarted).await?
                        }
                        Release::Releasing { .. } => {
                            execution::stop(ctx, op, StopReason::ReleaseRefused).await?
                        }
                        Release::Released { .. } => {}
                    }
                }
                let (adapter, ctx, id) = (self.clone(), ctx.clone(), op.id.clone());
                tokio::spawn(async move { adapter.observe(ctx, id, false, true).await });
                return Ok(ParkedRecovery::LeaveParked);
            }
            let timeout = op.parked_deadline_ms.is_some_and(|d| now_ms() > d)
                || matches!(record.release, Release::Released { authorized_at_ms } | Release::Releasing { authorized_at_ms }
                    if now_ms() >= authorized_at_ms + frozen.gate.timeout_secs_clamped() * 1000);
            if record.decision == Decision::Observing && timeout {
                execution::stop(ctx, op, StopReason::Timeout).await?;
            } else {
                crate::operation::owned_execution::drain_exited(ctx, op, &record.execution_id)
                    .await?;
            }
            return Ok(ParkedRecovery::LeaveParked);
        }
        if record.decision == Decision::Observing
            && !matches!(record.release, Release::Released { .. })
        {
            execution::stop(ctx, op, StopReason::ReleaseRefused).await?;
            return Ok(ParkedRecovery::LeaveParked);
        }
        let paths = self.paths(&frozen.task_id, frozen.attempt);
        let evidence = GateEvidence {
            log_path: paths.log,
            step_path: Some(paths.step),
            steps: frozen.gate.steps.clone(),
        };
        let drained = matches!(
            settled,
            Some(Settlement::Journal(
                ExecutionState::Drained { .. }
                    | ExecutionState::OriginalGroupEmpty { .. }
                    | ExecutionState::InterruptedQuiescent { .. }
            ))
        );
        let interrupted = match &settled {
            Some(settlement) => {
                execution::interrupted(ctx, &record.execution_id, settlement, &record.decision)
                    .await
            }
            None => false,
        };
        let verdict = if interrupted {
            infra_verdict(
                "execution interrupted without child status",
                &evidence,
                frozen.attempt,
            )
        } else {
            match record.decision {
                Decision::Stopping {
                    reason: StopReason::Timeout,
                } => timeout_verdict(
                    &evidence,
                    frozen.attempt,
                    frozen.gate.timeout_secs_clamped(),
                ),
                Decision::Stopping { .. } => infra_verdict(
                    "gate release refused or kernel restarted",
                    &evidence,
                    frozen.attempt,
                ),
                Decision::Observing => match settled {
                    Some(Settlement::Journal(
                        ExecutionState::Drained {
                            status: Some(code), ..
                        }
                        | ExecutionState::OriginalGroupEmpty { status: Some(code) },
                    )) => verdict_from_exit_code(code, &evidence, frozen.attempt),
                    _ => infra_verdict("execution has no child status", &evidence, frozen.attempt),
                },
            }
        };
        let stopped = if drained {
            Ok(())
        } else {
            Err(CalmError::Conflict("execution not Drained".into()))
        };
        let result = if interrupted {
            target::interrupted(verdict, &frozen)
        } else {
            target::finalize(verdict, &frozen, stopped).await
        };
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("task-verify-after-sample");
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &self.before_completion {
            hook().await;
        }
        Ok(ParkedRecovery::Complete(Box::new((
            crate::operation::ParkedOutcome::Succeeded {
                result: serde_json::to_value(result)?,
            },
            None,
        ))))
    }

    async fn observe(self, ctx: SpawnCtx, id: String, mut restart: bool, mut retry_close: bool) {
        #[cfg(test)]
        if let Some(hook) = &self.before_release {
            hook().await;
        }
        loop {
            if let Ok(Some(current)) = ctx.operation_repo.get_operation(&id).await {
                if matches!(
                    current.phase.tag(),
                    PhaseTag::Succeeded | PhaseTag::Failed | PhaseTag::Stuck
                ) {
                    self.owned_parked_completed(&current, &ctx);
                    return;
                }
                if let Ok(Some(op)) = ctx.operation_repo.claim_parked(&id).await {
                    let result: Result<()> = async {
                        let record = execution::record(&op)?;
                        if record.decision == Decision::Observing {
                            if retry_close
                                && matches!(record.release, Release::Released { .. })
                                && let Ok(engine) = execution::engine(&ctx)
                            {
                                // Boot retries only close; it never sends another go token.
                                retry_close =
                                    engine.close_stdin(&record.execution_id).await.is_err();
                            }
                            if restart {
                                execution::stop(&ctx, &op, StopReason::KernelRestarted).await?;
                                restart = false;
                            } else if record.release == Release::Held {
                                let frozen =
                                    FrozenVerify::from_output(super::super::required_output(&op)?)?;
                                if op.parked_deadline_ms.is_some_and(|d| now_ms() > d) {
                                    execution::stop(&ctx, &op, StopReason::Timeout).await?;
                                } else if execution::release(
                                    &ctx,
                                    &op,
                                    execution::ReleaseAuthority::Task(&frozen.task_id),
                                    #[cfg(any(test, feature = "fixtures"))]
                                    None,
                                )
                                .await
                                .is_err()
                                {
                                    let latest = execution::refresh_claim(&ctx, &op).await?;
                                    execution::stop(&ctx, &latest, StopReason::ReleaseRefused)
                                        .await?;
                                }
                            }
                        }
                        let latest = execution::refresh_claim(&ctx, &op).await?;
                        super::super::owned_parked::reconcile(
                            &self,
                            &latest,
                            RecoveryMode::PreDeadlineProbe,
                            &ctx,
                        )
                        .await
                    }
                    .await;
                    if let Err(error) = result {
                        tracing::warn!(op_id=%id,%error,"gate execution retained");
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

pub(super) fn legacy_error(op: &Operation) -> CalmError {
    CalmError::Conflict(format!(
        "pre-C2b task-verify unresolved; drain under the old kernel or manually stop pgid {}",
        op.spawn_artifacts.as_ref().map_or(0, |a| a.pgid)
    ))
}

pub(super) async fn require_prior_settled(
    tx: &mut super::super::Tx<'_>,
    task: &str,
    attempt: i64,
) -> Result<()> {
    use crate::operation::gate_ops::{
        AttemptGateOps, GateOpKind, classify_gate_ops, gate_ops_of_attempt_tx,
    };
    let mut prior = gate_ops_of_attempt_tx(tx, task).await?;
    prior.retain(|op| op.kind == GateOpKind::Verify && op.number < attempt);
    if !matches!(classify_gate_ops(&prior), AttemptGateOps::Clear) {
        return Err(CalmError::Conflict(
            "prior gate execution has not settled".into(),
        ));
    }
    Ok(())
}
