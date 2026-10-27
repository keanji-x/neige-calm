use super::*;

#[async_trait]
impl ProviderAdapter for CodexAdapter {
    async fn preparation_track(
        &self,
        input: &Value,
        _ctx: &crate::operation::SpawnCtx,
    ) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<CodexCreateOperationPayload>(input.clone())?
                .request
                .track_id,
        ))
    }

    fn kind(&self) -> &'static str {
        "codex-create"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CODEX_PHASES
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        if output_prompt(output)?.is_some() {
            Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
        } else {
            Ok(AppServerInteractKind::RegisterPending { entry_id: None })
        }
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: CodexCreateOperationPayload = serde_json::from_value(input.clone())?;
        if payload.request.cwd.chars().any(|c| c.is_ascii_control()) {
            return Err(CalmError::BadRequest(
                "cwd must not contain ASCII control characters".into(),
            ));
        }
        validate_optional_color(payload.request.icon_bg.as_deref(), "icon_bg")?;
        validate_optional_color(payload.request.icon_fg.as_deref(), "icon_fg")?;
        if self
            .repo
            .track_get(&payload.request.track_id)
            .await?
            .is_none()
        {
            return Err(CalmError::NotFound(format!(
                "track {}",
                payload.request.track_id
            )));
        }
        if !self.shared_codex_appserver.is_running() {
            // Message carries the live failure and the background-retry fact; preflights stay non-blocking.
            return Err(self.shared_codex_appserver.not_running_error());
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CodexCreateOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let runtime_id = payload.worker_session_id.clone().unwrap_or_else(new_id);
        let track_id = payload.request.track_id.clone();
        let env = build_codex_env(self.repo.as_ref(), self.codex.as_ref(), &card_id).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            TrackId::from(track_id.clone()),
        )
        .await?;
        let (card, term, _token) = card_with_codex_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            None,
            TrackId::from(track_id.clone()),
            payload.request.title.clone(),
            payload.request.sort,
            payload.request.cwd.clone(),
            env.clone(),
            payload.request.prompt.clone(),
            payload.request.icon_bg.clone(),
            payload.request.icon_fg.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            payload.request.theme,
        )
        .await?;
        let projected_card =
            project_codex_runtime_fields_for_response(card.clone(), Some(&term.id), None, None);
        let event = Event::CardAdded(projected_card.clone());
        let runtime_event = Event::WorkerSessionStarted {
            worker_session_id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
        };
        let event_id =
            append_decision_event_in_tx(tx, &payload.actor, &scope, None, &event).await?;
        let runtime_event_id =
            append_decision_event_in_tx(tx, &payload.actor, &scope, None, &runtime_event).await?;

        let mut output = TxOutput::new(
            "runtime",
            Some(runtime_id.clone()),
            serde_json::to_value(&projected_card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "track_id": card.track_id,
            "terminal_id": term.id,
            "cwd": payload.request.cwd,
            "env": env,
            "prompt": payload.request.prompt,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor.clone(),
            scope: scope.clone(),
            event,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: runtime_event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor,
            scope,
            event: runtime_event,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let expected_output = output.clone();
        let payload: CodexCreateOperationPayload = serde_json::from_value(op.payload.clone())?;
        let card_id = output.output_string("card_id", "codex")?;
        let cwd = output.output_string("cwd", "codex")?;

        if let Some(prompt_text) = output_prompt(output)? {
            let mut notifs = self.shared_codex_appserver.subscribe_notifications();
            let thread_id = match output
                .output_optional_string("codex_thread_id", "codex")?
                .or_else(|| phase_minted_thread_id(&op.phase))
            {
                Some(thread_id) => thread_id,
                None => {
                    if let Some(runtime) = self
                        .repo
                        .session_projection_active_for_card(&card_id)
                        .await?
                        && let Some(thread_id) =
                            TxOutput::non_empty_string(runtime.thread_id.as_deref())
                    {
                        thread_id
                    } else {
                        // `ThreadConfig::NoMcp` is correct here: the interactive card injects no NEIGE_MCP_SOCKET/NEIGE_MCP_TOKEN and has no worker prompt,
                        // so there is nothing to put in `shell_environment_policy.set`; only the worker path needs it.
                        self.shared_codex_appserver
                            .thread_start_mint_for_card(
                                &card_id,
                                SharedThreadStartParams {
                                    cwd: cwd.clone(),
                                    approval_policy: "never".into(),
                                    sandbox_mode: "workspace-write".into(),
                                    developer_instructions: None,
                                    config: ThreadConfig::NoMcp,
                                },
                            )
                            .await?
                    }
                }
            };
            output.set_output_data("codex_thread_id", json!(thread_id.clone()), "codex")?;
            let updated = persist_prompt_thread(
                ctx,
                &self.card_role_cache,
                &self.track_area_cache,
                op,
                &expected_output,
                output,
                payload.actor.clone(),
                &card_id,
                &thread_id,
            )
            .await?;
            output.result = serde_json::to_value(&updated)?;
            let turn_started_at_ms = output_optional_i64(output, "turn_started_at_ms")?;
            if turn_started_at_ms.is_none() {
                let expected_output = output.clone();
                self.shared_codex_appserver
                    // Worker/dedicated threads carry no per-card model selection (the picker addresses planner cards only).
                    .turn_start(
                        &thread_id,
                        vec![InputItem::text(prompt_text)],
                        &TurnModelSelection::inherit(),
                        TurnApprovals::Unchanged,
                        None,
                    )
                    .await?;
                output.set_output_data("turn_started_at_ms", json!(now_ms()), "codex")?;
                checkpoint_prompt_turn_started(ctx, op, &expected_output, output, &thread_id)
                    .await?;
            }
            match await_shared_initial_turn_lifecycle(
                &mut notifs,
                &thread_id,
                self.initial_turn_lifecycle_timeout,
            )
            .await
            {
                Ok(()) => {}
                Err(err) => return Err(err),
            }
            Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
        } else {
            self.shared_codex_appserver
                .ensure_respawn_for_current_settings()
                .await?;
            output.set_output_data("pending_registered", json!(true), "codex")?;
            let updated = persist_pending_thread_status(
                ctx,
                &self.card_role_cache,
                &self.track_area_cache,
                op,
                &expected_output,
                output,
                payload.actor,
                &card_id,
            )
            .await?;
            output.result = serde_json::to_value(&updated)?;
            Ok(
                AppServerInteractOutcome::RegisteredPendingForLaterAttribution {
                    entry_id: card_id,
                },
            )
        }
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let card_id = output.output_string("card_id", "codex")?;
        let track_id = output.output_string("track_id", "codex")?;
        let runtime_id = output.output_string("runtime_id", "codex")?;
        let cwd = output.output_string("cwd", "codex")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        let is_prompted = output_prompt(output)?.is_some();
        let command_line = if is_prompted {
            let thread_id = output.output_string("codex_thread_id", "codex")?;
            format!(
                "codex resume {} --remote {}",
                sh_single_quote(&thread_id),
                sh_single_quote(&self.shared_codex_appserver.remote_uri()),
            )
        } else {
            format!(
                "codex --remote {}",
                sh_single_quote(&self.shared_codex_appserver.remote_uri()),
            )
        };

        if !is_prompted {
            let _pending_spawn_serial_guard = self.pending_codex_threads_spawn_serial.lock().await;
            self.pending_codex_threads
                .register(PendingEntry::new(
                    card_id,
                    Some(track_id),
                    terminal_id.clone(),
                    runtime_id.clone(),
                ))
                .await?;

            #[cfg(feature = "fixtures")]
            if let Some(hook) = &self.spawn_hook {
                return hook(terminal_id, command_line, cwd, env)
                    .await
                    .map(SpawnOutcome::Ready);
            }

            return ctx
                .spawn_terminal(&term, &command_line, &cwd, &env)
                .await
                .map(SpawnOutcome::Ready);
        }

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.spawn_hook {
            return hook(terminal_id, command_line, cwd, env)
                .await
                .map(SpawnOutcome::Ready);
        }

        ctx.spawn_terminal(&term, &command_line, &cwd, &env)
            .await
            .map(SpawnOutcome::Ready)
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let card_id = output.output_string("card_id", "codex")?;
        let terminal_id = output.output_string("terminal_id", "codex")?;
        let mut steps = vec![CompensationStep::new(
            "reap_terminal_pty",
            json!({ "terminal_id": terminal_id }),
        )];
        if output_prompt(output)?.is_some() {
            steps.push(CompensationStep::new(
                "interrupt_shared_codex_turn",
                json!({
                    "card_id": card_id.clone(),
                    "thread_id": output.output_optional_string("codex_thread_id", "codex")?,
                }),
            ));
            steps.push(CompensationStep::new(
                "session_projection_set_status_failed_for_card",
                json!({ "card_id": card_id }),
            ));
        } else {
            let runtime_id = output.output_string("runtime_id", "codex")?;
            steps.push(CompensationStep::new(
                "pending_codex_threads_remove_by_card",
                json!({ "card_id": card_id.clone(), "runtime_id": runtime_id }),
            ));
            steps.push(CompensationStep::new(
                "card_payload_clear_pending_status",
                json!({ "card_id": card_id }),
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
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "reap_terminal_pty" => {
                let terminal_id = step.arg_string("terminal_id", "codex")?;
                if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
                    try_reap_terminal_artifacts_with_renderer(
                        Some(ctx.terminal_renderer.as_ref()),
                        &term,
                    )
                    .await?;
                }
                Ok(())
            }
            "interrupt_shared_codex_turn" => {
                let card_id = step.arg_string("card_id", "codex")?;
                let thread_id =
                    if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str) {
                        Some(thread_id.to_string())
                    } else {
                        ctx.repo
                            .session_projection_active_for_card(&card_id)
                            .await?
                            .and_then(|runtime| {
                                TxOutput::non_empty_string(runtime.thread_id.as_deref())
                            })
                    };
                if let Some(thread_id) = thread_id {
                    match self
                        .shared_codex_appserver
                        .interrupt_active_outcome(&thread_id)
                        .await
                    {
                        NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => {}
                        NativeInterruptOutcome::Unconfirmed { error: e } => {
                            tracing::warn!(
                                card_id = %card_id,
                                thread_id = %thread_id,
                                error = %e,
                                "prompted codex compensation could not interrupt active shared turn"
                            );
                        }
                    }
                }
                Ok(())
            }
            // Back-compat: accept the legacy op string during recovery so in-flight compensation states still drain.
            "session_projection_set_status_failed_for_card"
            | "runtime_set_status_failed_for_card" => {
                let card_id = step.arg_string("card_id", "codex")?;
                ctx.repo
                    .session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
                    .await?;
                Ok(())
            }
            "pending_codex_threads_remove_by_card" => {
                let runtime_id = output.output_string("runtime_id", "codex")?;
                self.pending_codex_threads
                    .remove_by_runtime(&runtime_id)
                    .await;
                Ok(())
            }
            "card_payload_clear_pending_status" => {
                let card_id = step.arg_string("card_id", "codex")?;
                let runtime_id = output.output_string("runtime_id", "codex")?;
                match crate::pending_codex_threads::card_payload_clear_pending_status(
                    ctx.repo.as_ref(),
                    &ctx.events,
                    &card_id,
                    &runtime_id,
                )
                .await
                {
                    Ok(()) | Err(CalmError::NotFound(_)) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            "delete_card_codex_thread" => {
                tracing::warn!(
                    target = "codex_adapter::compensation",
                    "skipping legacy delete_card_codex_thread step (table dropped post #552)"
                );
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "unknown codex compensation op {other}"
            ))),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_prompt_thread(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    op: &Operation,
    expected_output: &TxOutput,
    output: &TxOutput,
    actor: ActorId,
    card_id: &str,
    thread_id: &str,
) -> Result<Card> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

    let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.track_id.clone()).await?;
    let card_id_for_tx = card_id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let card_for_event = card;
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let expected_output_for_tx = expected_output.clone();
    let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
    let (updated, _ids) = write_with_events_typed(
        ctx.repo.as_ref(),
        actor,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let runtime = session_projection_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to bind"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                session_bind_attribution_tx(
                    tx,
                    &runtime.id,
                    ThreadAttribution {
                        worker_session_id: runtime.id.clone(),
                        provider: AgentProvider::Codex,
                        thread_id: Some(thread_id_for_tx.clone()),
                        session_id: None,
                        active_turn_id: None,
                    },
                )
                .await?;
                let old_status = runtime.status;
                let runtime_id = runtime.id.clone();
                if runtime.status != WorkerSessionState::Running {
                    session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
                }
                let card = project_codex_runtime_fields_for_response(
                    card_for_event,
                    terminal_id_for_projection.as_deref(),
                    Some(&thread_id_for_tx),
                    Some("started"),
                );
                let mut checkpoint_output = output_for_tx.clone();
                checkpoint_output.result = serde_json::to_value(&card)?;
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::MintAndAwait {
                        thread_id: Some(thread_id_for_tx.clone()),
                    },
                    &expected_output_for_tx,
                    &checkpoint_output,
                )
                .await?;
                let mut events = vec![(scope.clone(), Event::CardUpdated(card.clone()))];
                if old_status != WorkerSessionState::Running {
                    events.push((
                        scope,
                        Event::WorkerSessionStatusChanged {
                            worker_session_id: runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: WorkerSessionState::Running,
                        },
                    ));
                }
                Ok((card, events))
            })
        },
    )
    .await?;
    Ok(updated)
}

async fn checkpoint_prompt_turn_started(
    ctx: &SpawnCtx,
    op: &Operation,
    expected_output: &TxOutput,
    output: &TxOutput,
    thread_id: &str,
) -> Result<()> {
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let expected_output_for_tx = expected_output.clone();
    let thread_id_for_tx = thread_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            checkpoint_app_server_interact_tx(
                tx,
                &op_for_tx,
                AppServerInteractKind::MintAndAwait {
                    thread_id: Some(thread_id_for_tx),
                },
                &expected_output_for_tx,
                &output_for_tx,
            )
            .await
        })
    })
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "Explicit prior and proposed outputs preserve checkpoint ownership"
)]
async fn persist_pending_thread_status(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    op: &Operation,
    expected_output: &TxOutput,
    output: &TxOutput,
    actor: ActorId,
    card_id: &str,
) -> Result<Card> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

    let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.track_id.clone()).await?;
    let card_id_for_tx = card_id.to_string();
    let card_for_event = card;
    let op_for_tx = op.clone();
    let output_for_tx = output.clone();
    let expected_output_for_tx = expected_output.clone();
    let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
    let (updated, _ids) = write_with_events_typed(
        ctx.repo.as_ref(),
        actor,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let runtime = session_projection_active_for_card_tx(tx, &card_id_for_tx)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "codex card {card_id_for_tx} has no active runtime to mark pending"
                        ))
                    })?;
                let terminal_id_for_projection = runtime.terminal_run_id.clone();
                let old_status = runtime.status;
                let runtime_id = runtime.id.clone();
                if old_status != WorkerSessionState::TurnPending {
                    session_set_status_tx(tx, &runtime.id, WorkerSessionState::TurnPending).await?;
                }
                let card = project_codex_runtime_fields_for_response(
                    card_for_event,
                    terminal_id_for_projection.as_deref(),
                    None,
                    Some("pending_thread_start"),
                );
                let mut checkpoint_output = output_for_tx.clone();
                checkpoint_output.result = serde_json::to_value(&card)?;
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::RegisterPending {
                        entry_id: Some(card_id_for_tx.clone()),
                    },
                    &expected_output_for_tx,
                    &checkpoint_output,
                )
                .await?;
                let mut events = vec![(scope.clone(), Event::CardUpdated(card.clone()))];
                if old_status != WorkerSessionState::TurnPending {
                    events.push((
                        scope,
                        Event::WorkerSessionStatusChanged {
                            worker_session_id: runtime_id,
                            card_id: card_id_for_tx,
                            old_status,
                            new_status: WorkerSessionState::TurnPending,
                        },
                    ));
                }
                Ok((card, events))
            })
        },
    )
    .await?;
    Ok(updated)
}

fn project_codex_runtime_fields_for_response(
    mut card: Card,
    terminal_id: Option<&str>,
    thread_id: Option<&str>,
    thread_status: Option<&str>,
) -> Card {
    if let Some(map) = card.payload.as_object_mut() {
        if let Some(terminal_id) = terminal_id {
            insert_payload_string(map, "terminal_id", terminal_id);
        }
        if let Some(thread_id) = thread_id {
            insert_payload_string(map, "codex_thread_id", thread_id);
        }
        if let Some(thread_status) = thread_status {
            insert_payload_string(map, "codex_thread_status", thread_status);
        }
    }
    card
}

fn insert_payload_string(map: &mut serde_json::Map<String, Value>, key: &str, value: &str) {
    map.entry(key.to_string())
        .or_insert_with(|| Value::String(value.to_string()));
}
