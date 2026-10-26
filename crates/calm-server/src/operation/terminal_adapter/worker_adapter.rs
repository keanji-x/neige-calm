//! Pipe preparation and Legacy row recovery for terminal task operations.
use super::*;

#[async_trait]
impl ProviderAdapter for TerminalWorkerAdapter {
    async fn preparation_cwd(&self, input: &Value, _ctx: &SpawnCtx) -> Result<Option<String>> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        Ok(explicit_terminal_cwd(payload.cwd))
    }
    async fn preparation_track(
        &self,
        input: &Value,
        _ctx: &crate::operation::SpawnCtx,
    ) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<TerminalWorkerOperationPayload>(input.clone())?.track_id,
        ))
    }

    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn crate::operation::PreparedTx>>> {
        let prepared = self.prepare_pipe_environment(op, ctx).await?;
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        target::prepare(ctx, &payload.track_id, explicit_terminal_cwd(payload.cwd)).await?;
        Ok(prepared)
    }

    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        pipe_worker::PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "terminal worker idempotency_key must not be empty".into(),
            ));
        }
        if self.repo.track_get(&payload.track_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("track {}", payload.track_id)));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        Err(CalmError::Internal(
            "terminal worker requires environment preflight".into(),
        ))
    }

    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some(pipe_worker::KEY)
    }

    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: crate::operation::RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<crate::operation::ParkedRecovery> {
        self.recover_pipe(op, mode, ctx).await
    }
    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<BroadcastEnvelope>> {
        self.complete_pipe_tx(tx, op).await
    }
    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(record) = crate::operation::owned_execution::pipe::record(pipe_worker::CONFIG, op)
        {
            ctx.execution.drive([record.execution_id]);
        }
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        if pipe_worker::is_pipe(output) {
            return self.spawn_pipe(output, _op, ctx).await;
        }
        let card_id = output_card_id(output)?;
        let terminal_id = output.output_string("terminal_id", "terminal")?;
        let track_id = TrackId::from(output.output_string("track_id", "terminal")?);
        let cmd = output.output_string("cmd", "terminal")?;
        let cwd = output.output_string("cwd", "terminal")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        crate::operation::terminal_launch::require_not_rejected(&output.data, &terminal_id)?;
        let existing_term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        if existing_term.exit_code.is_some() || existing_term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?existing_term.exit_code,
                signal_killed = existing_term.signal_killed,
                "terminal-worker recovery: worker already exited; skipping respawn",
            );
            log_terminal_worker_card_added(
                ctx,
                &self.card_role_cache,
                &self.track_area_cache,
                &card_id,
                &track_id,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::error!(
                    card_id = %card_id,
                    track_id = %track_id,
                    error = %e,
                    "terminal worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(_op.payload.clone())?;
        crate::operation::admit_task_side_effect(ctx.repo.as_ref(), &payload.idempotency_key)
            .await?;
        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.preparation_hook {
            hook().await;
        }
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;

        let launch = crate::operation::task_launch::TaskLaunch::new(
            &payload.idempotency_key,
            _op,
            crate::operation::task_launch::TerminalLaunchRole::BusinessProcess,
        );
        let spawn_result = if let Some(hook) = &self.spawn_hook {
            launch
                .run(
                    ctx.repo.as_ref(),
                    hook(terminal_id.clone(), cmd.clone(), cwd.clone(), env.clone()),
                )
                .await
        } else {
            ctx.spawn_task_terminal(&term, &cmd, &cwd, &env, launch)
                .await
        };

        match spawn_result {
            Ok(handle) => {
                if let Err(e) = ctx
                    .repo
                    .session_projection_set_status_for_card(
                        card_id.as_ref(),
                        WorkerSessionState::Running,
                    )
                    .await
                {
                    tracing::warn!(
                        target: "operation::terminal_worker_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark terminal worker runtime running after spawn; CardAdded still broadcasting",
                    );
                }
                log_terminal_worker_card_added(
                    ctx,
                    &self.card_role_cache,
                    &self.track_area_cache,
                    &card_id,
                    &track_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        track_id = %track_id,
                        error = %e,
                        "terminal worker CardAdded append failed after live spawn; continuing"
                    );
                });
                Ok(SpawnOutcome::Ready(handle))
            }
            Err(e) if worker_spawn_failure_preserved(ctx.repo.as_ref(), &terminal_id).await? => {
                tracing::info!(
                    card_id = %card_id,
                    track_id = %track_id,
                    terminal_id = %terminal_id,
                    spawn_err = %e,
                    "worker terminal fast-exit (sidecar present); preserving card + terminal",
                );
                log_terminal_worker_card_added(
                    ctx,
                    &self.card_role_cache,
                    &self.track_area_cache,
                    &card_id,
                    &track_id,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        card_id = %card_id,
                        track_id = %track_id,
                        error = %e,
                        "terminal worker CardAdded append failed after fast-exit preservation; continuing"
                    );
                });
                Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
            }
            Err(e) => Err(e),
        }
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        if pipe_worker::is_pipe(output) {
            return Ok(CompensationStateVersioned {
                version: 1,
                from_phase,
                reason: reason.into(),
                steps: vec![CompensationStep::new("cleanup_pipe_worker", json!({}))],
            });
        }
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep {
                op: "cleanup_terminal_worker".into(),
                args: json!({
                    "card_id": output.output_string("card_id", "terminal")?,
                    "terminal_id": output.output_string("terminal_id", "terminal")?,
                }),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if pipe_worker::is_pipe(output) {
            return self.compensate_pipe(op, ctx).await;
        }
        crate::operation::worker_cleanup::require_cleanup_safe(ctx, op, output, false).await?;
        if step.op != "cleanup_terminal_worker" {
            return Err(CalmError::Internal(format!(
                "unknown terminal worker compensation op {}",
                step.op
            )));
        }
        let card_id = step
            .args
            .get("card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("terminal worker cleanup missing card_id".into()))?;
        let terminal_id = step
            .args
            .get("terminal_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("terminal worker cleanup missing terminal_id".into())
            })?;
        compensate_worker_rows(
            &ctx.execution,
            ctx.repo.as_ref(),
            ctx.terminal_renderer.as_ref(),
            &self.card_role_cache,
            card_id,
            terminal_id,
        )
        .await?;
        Ok(())
    }
}
