use super::*;

#[async_trait]
impl ProviderAdapter for CodexWorkerAdapter {
    async fn preparation_track(
        &self,
        input: &Value,
        _ctx: &crate::operation::SpawnCtx,
    ) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<CodexWorkerOperationPayload>(input.clone())?.track_id,
        ))
    }

    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn crate::operation::PreparedTx>>> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::operation::workspace_lease::worker::preparation::collect(
            self.clone(),
            op,
            ctx,
            &payload.track_id,
            &payload.idempotency_key,
            &self.workspace_root,
        )
        .await
    }

    fn kind(&self) -> &'static str {
        "codex-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::AppServerInteract,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Succeeded,
        ]
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::RegisterPending {
            entry_id: Some(output.output_string("card_id", "codex-worker")?),
        })
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        if payload.idempotency_key.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "codex worker idempotency_key must not be empty".into(),
            ));
        }
        if self.repo.track_get(&payload.track_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("track {}", payload.track_id)));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::operation::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        Err(CalmError::Internal(
            "worker checkout requires external preparation".into(),
        ))
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let payload: CodexWorkerOperationPayload = serde_json::from_value(op.payload.clone())?;
        crate::operation::admit_task_side_effect(ctx.repo.as_ref(), &payload.idempotency_key)
            .await?;
        verify_codex_worker_workspace(
            ctx,
            &self.card_role_cache,
            &self.track_area_cache,
            op,
            output,
        )
        .await?;
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let card_id = output.output_string("card_id", "codex")?;
        let session_id = output.output_string("runtime_id", "codex")?;
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let track_id = TrackId::from(output.output_string("track_id", "codex")?);
        let cwd = output.output_string("cwd", "codex")?;
        let rendered_prompt = output.output_string("prompt", "codex")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        if term.exit_code.is_some() || term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?term.exit_code,
                signal_killed = term.signal_killed,
                "codex-worker recovery: worker already exited; skipping respawn",
            );
            log_worker_card_added(
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
                    "codex worker CardAdded append failed after recovery exit preservation; continuing"
                );
            });
            return Ok(SpawnOutcome::Ready(SpawnHandle::NoOp));
        }

        let payload: CodexWorkerOperationPayload = serde_json::from_value(_op.payload.clone())?;
        crate::operation::admit_task_side_effect(ctx.repo.as_ref(), &payload.idempotency_key)
            .await?;
        // #1933: a `SpawnStarted` re-drive reaches here without `app_server_interact`.
        verify_recorded_head(output, "codex-worker")?;
        let shared = self.codex_backends.for_session(&session_id).await?;
        if !shared.is_running() {
            return Err(shared.not_running_error());
        }

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.preparation_hook {
            hook().await;
        }
        let card = ctx
            .repo
            .card_get(&card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
        let mcp_token = mint_card_mcp_token(ctx, &card_id, &session_id).await?;

        let handle = spawn_codex_worker_via_shared_daemon(CodexWorkerSpawnCtx {
            spawn_ctx: ctx,
            launch: crate::operation::task_launch::TaskLaunch::new(
                &payload.idempotency_key,
                _op,
                crate::operation::task_launch::TerminalLaunchRole::OptionalViewer,
            ),
            #[cfg(test)]
            viewer_preparation_hook: self.viewer_preparation_hook.as_ref(),
            shared_codex_appserver: shared,
            mcp_server: self.mcp_server.as_deref(),
            card: &card,
            term: &term,
            worker_session_id: &session_id,
            track_id: &track_id,
            mcp_token: Some(mcp_token.as_str()),
            rendered_prompt: &rendered_prompt,
            cwd: &cwd,
            legacy_env: &env,
        })
        .await?;

        log_worker_card_added(
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
                "codex worker CardAdded append failed after live spawn; continuing"
            );
        });
        Ok(SpawnOutcome::Ready(handle))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        // The lease is released last, after the worker rows are gone (#1830 S2 D7): its
        // `spawn-failed` delivery commits whatever the attempt left in the track's checkout.
        let mut steps = vec![CompensationStep::new(
            "cleanup_codex_worker",
            json!({
                "card_id": output.output_string("card_id", "codex")?,
                "terminal_id": output.output_string("terminal_id", "codex")?,
            }),
        )];
        if let Some(lease_id) = output.output_optional_string("lease_id", "codex")? {
            steps.push(CompensationStep::new(
                "release_workspace_lease",
                json!({ "lease_id": lease_id }),
            ));
        }
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps,
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
        let mut business_may_be_live = false;
        if crate::operation::worker_cleanup::may_have_started(op)? {
            let worker_session_id = output.output_string("runtime_id", "codex cleanup")?;
            if let Some(session) = ctx
                .repo
                .session_projection_by_id(&worker_session_id)
                .await?
                && let Some(thread_id) = session.thread_id.as_deref()
            {
                let shared = self.codex_backends.for_session(&worker_session_id).await?;
                business_may_be_live = true;
                // An interrupt request is not proof of stopped external writers.
                match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    shared.interrupt_active_outcome(thread_id),
                )
                .await
                {
                    Ok(
                        NativeInterruptOutcome::NativeAck
                        | NativeInterruptOutcome::NoTarget
                        | NativeInterruptOutcome::Unconfirmed { .. },
                    )
                    | Err(_) => {}
                }
            }
        }
        crate::operation::worker_cleanup::require_cleanup_safe(
            ctx,
            op,
            output,
            business_may_be_live,
        )
        .await?;
        if step.op == "release_workspace_lease" {
            let lease_id = step.arg_string("lease_id", "codex")?;
            let pool = ctx.operation_repo.sqlite_db();
            release_workspace_lease_by_id(
                &pool,
                &ctx.events,
                &lease_id,
                ReleaseDelivery::Commit(AttemptOutcome::SpawnFailed),
            )
            .await?;
            return Ok(());
        }
        if step.op != "cleanup_codex_worker" {
            return Err(CalmError::Internal(format!(
                "unknown codex worker compensation op {}",
                step.op
            )));
        }
        let card_id = step.arg_string("card_id", "codex")?;
        let terminal_id = step.arg_string("terminal_id", "codex")?;
        let worker_session_id = output.output_string("runtime_id", "codex")?;
        let runtime_turn = ctx
            .repo
            .session_projection_by_id(&worker_session_id)
            .await?
            .and_then(|runtime| {
                TxOutput::non_empty_string(runtime.thread_id.as_deref()).map(|thread_id| {
                    (
                        thread_id,
                        TxOutput::non_empty_string(runtime.active_turn_id.as_deref()),
                    )
                })
            });
        let shared = if runtime_turn.is_some() {
            Some(self.codex_backends.for_session(&worker_session_id).await?)
        } else {
            None
        };
        let outcome = compensate_worker_rows(
            &ctx.execution,
            ctx.repo.as_ref(),
            ctx.terminal_renderer.as_ref(),
            &self.card_role_cache,
            &card_id,
            &terminal_id,
        )
        .await?;
        if outcome == WorkerCleanupOutcome::Deleted
            && let Some((thread_id, persisted_turn)) = runtime_turn
        {
            let shared = shared.expect("runtime turn resolved before row cleanup");
            let cached_turn = shared.active_turn_id_for_thread(&thread_id);
            match shared.interrupt_active_outcome(&thread_id).await {
                NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => {}
                NativeInterruptOutcome::Unconfirmed { error: e } => {
                    let turn_id = cached_turn
                        .as_deref()
                        .or(persisted_turn.as_deref())
                        .unwrap_or("");
                    tracing::warn!(
                        worker_session_id = %worker_session_id,
                        thread_id = %thread_id,
                        turn_id = %turn_id,
                        error = %e,
                        "worker compensation replay thread interrupt failed"
                    );
                }
            }
            if cached_turn.is_none()
                && let Some(persisted_turn) = persisted_turn.as_deref()
            {
                match shared.interrupt_outcome(&thread_id, persisted_turn).await {
                    NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => {}
                    NativeInterruptOutcome::Unconfirmed { error: e } => {
                        tracing::warn!(
                            worker_session_id = %worker_session_id,
                            thread_id = %thread_id,
                            turn_id = persisted_turn,
                            error = %e,
                            "worker compensation replay persisted-turn interrupt failed"
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
