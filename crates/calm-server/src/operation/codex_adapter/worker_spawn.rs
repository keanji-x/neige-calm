use super::*;

pub(crate) struct CodexWorkerSpawnCtx<'a> {
    pub(crate) spawn_ctx: &'a SpawnCtx,
    pub(crate) launch: crate::operation::task_launch::TaskLaunch,
    #[cfg(test)]
    pub(crate) viewer_preparation_hook:
        Option<&'a Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    pub(crate) shared_codex_appserver: &'a Arc<SharedCodexAppServer>,
    pub(crate) mcp_server: Option<&'a McpServer>,
    pub(crate) card: &'a Card,
    pub(crate) term: &'a crate::model::Terminal,
    pub(crate) worker_session_id: &'a str,
    pub(crate) track_id: &'a TrackId,
    pub(crate) mcp_token: Option<&'a str>,
    pub(crate) rendered_prompt: &'a str,
    pub(crate) cwd: &'a str,
    pub(crate) legacy_env: &'a Value,
}

pub(crate) async fn spawn_codex_worker_via_shared_daemon(
    ctx: CodexWorkerSpawnCtx<'_>,
) -> Result<SpawnHandle> {
    let mut notifications = ctx.shared_codex_appserver.subscribe_notifications();
    let remote_uri = ctx.shared_codex_appserver.remote_uri();
    let card_id = ctx.card.id.as_str();
    let runtime_id = ctx.worker_session_id.to_string();
    let runtime = ctx
        .spawn_ctx
        .repo
        .session_projection_by_id(&runtime_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("worker runtime {runtime_id} vanished")))?;
    let persisted_turn_id = TxOutput::non_empty_string(runtime.active_turn_id.as_deref());
    let worker_instructions = crate::planner_card::render_system_prompt(
        crate::planner_card::SeededCardRole::WorkerCodex.prompt_template(),
        ctx.track_id.as_str(),
    );
    let (thread_id, minted) =
        if let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref()) {
            (thread_id, None)
        } else {
            // codex does NOT inherit the daemon process env into exec-shells (and the daemon `env_remove`s NEIGE_MCP_TOKEN from itself); the per-thread
            // `shell_environment_policy.set` is the only channel. Both arms are always `Some` for a real worker spawn; `from_parts` test hatches must wire a stub `McpServer`.
            let (token, server) = match (ctx.mcp_token, ctx.mcp_server) {
                (Some(token), Some(server)) => (token, server),
                _ => {
                    return Err(CalmError::Internal(
                        "codex worker shared-daemon spawn requires MCP server and per-card token"
                            .into(),
                    ));
                }
            };
            let minted = ctx
                .shared_codex_appserver
                .thread_start_mint_mcp_shell(
                    card_id,
                    ctx.cwd.to_string(),
                    Some(worker_instructions),
                    server.shim_config.socket_path.clone(),
                    token.to_string(),
                )
                .await?;
            tracing::info!(
                target: "shared_codex_daemon::worker",
                card_id,
                track_id = %ctx.track_id,
                thread_id = %minted.thread_id,
                "thread_start_succeeded"
            );
            (minted.thread_id.clone(), Some(minted))
        };

    persist_shared_worker_runtime_fields(
        ctx.spawn_ctx,
        ctx.card,
        ctx.worker_session_id,
        &thread_id,
        minted.as_ref(),
        &remote_uri,
        persisted_turn_id.as_deref(),
    )
    .await?;

    if persisted_turn_id.is_none() {
        let initial_turn_result = async {
            let shared = Arc::clone(ctx.shared_codex_appserver);
            let launch_thread = thread_id.clone();
            let items = vec![InputItem::text(ctx.rendered_prompt.trim())];
            let started = ctx
                .launch
                .clone()
                .run_observed(ctx.spawn_ctx.repo.as_ref(), async move {
                    shared
                        .turn_start(
                            &launch_thread,
                            items,
                            &TurnModelSelection::inherit(),
                            TurnApprovals::Unchanged,
                            None,
                        )
                        .await
                })
                .await;
            let turn_id = match started {
                Ok(turn_id) => turn_id,
                Err(failure) => return Err(failure.error),
            };
            persist_shared_worker_runtime_fields(
                ctx.spawn_ctx,
                ctx.card,
                ctx.worker_session_id,
                &thread_id,
                None,
                &remote_uri,
                Some(&turn_id),
            )
            .await?;
            await_shared_worker_initial_turn_started(&mut notifications, &thread_id).await?;
            Ok::<(), CalmError>(())
        }
        .await;
        if let Err(e) = initial_turn_result {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                card_id,
                track_id = %ctx.track_id,
                thread_id = %thread_id,
                error = %e,
                "turn_start_failed"
            );
            return Err(e);
        }
    }

    // The business turn is already issued; a viewer-preparation failure must not route into business startup compensation.
    match ctx.spawn_ctx.repo.task_get(ctx.launch.task_id()).await {
        Ok(Some(task)) if !task.status.is_terminal() => {}
        Ok(_) => return Ok(SpawnHandle::NoOp),
        Err(error) => {
            tracing::warn!(card_id, %error, "optional viewer task read unavailable after business turn; retaining execution");
            return Ok(SpawnHandle::NoOp);
        }
    }
    #[cfg(test)]
    if let Some(hook) = ctx.viewer_preparation_hook {
        hook().await;
    }
    let mut env_for_spawn = ctx.legacy_env.clone();
    if let Some(map) = env_for_spawn.as_object_mut() {
        map.insert(
            "CODEX_HOME".into(),
            Value::String(ctx.shared_codex_appserver.status_snapshot().codex_home),
        );
        if let (Some(token), Some(server)) = (ctx.mcp_token, ctx.mcp_server) {
            for (key, value) in card_mcp_env(&server.shim_config.socket_path, token) {
                map.insert(key.into(), Value::String(value));
            }
        }
    }

    let command_line = format!(
        "codex resume {} --remote {}",
        sh_single_quote(&thread_id),
        sh_single_quote(&remote_uri)
    );
    match ctx
        .spawn_ctx
        .spawn_task_terminal(
            ctx.term,
            &command_line,
            ctx.cwd,
            &env_for_spawn,
            ctx.launch.clone(),
        )
        .await
    {
        Ok(handle) => {
            tracing::info!(
                target: "shared_codex_daemon::worker",
                card_id,
                track_id = %ctx.track_id,
                terminal_id = %ctx.term.id,
                thread_id = %thread_id,
                "worker_spawn_succeeded"
            );
            Ok(handle)
        }
        Err(error) => {
            tracing::warn!(card_id, thread_id=%thread_id, %error, "optional worker viewer unavailable after business turn; retaining execution");
            Ok(SpawnHandle::NoOp)
        }
    }
}

pub(super) async fn mint_card_mcp_token(
    ctx: &SpawnCtx,
    card_id: &str,
    runtime_id: &str,
) -> Result<String> {
    let card_id = card_id.to_string();
    let runtime_id = runtime_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        let card_id = card_id.clone();
        let runtime_id = runtime_id.clone();
        Box::pin(async move { mint_and_persist_card_token(tx, &card_id, &runtime_id).await })
    })
    .await
}

pub(super) async fn log_worker_card_added(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    card_id: &str,
    track_id: &TrackId,
) -> Result<()> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.to_string()),
        track_id.clone(),
    )
    .await?;
    ctx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            scope,
            None,
            &ctx.events,
            card_role_cache,
            track_area_cache,
            Event::CardAdded(card),
        )
        .await?;
    Ok(())
}

async fn await_shared_worker_initial_turn_started(
    rx: &mut tokio::sync::broadcast::Receiver<crate::codex_appserver::Notification>,
    thread_id: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                thread_id,
                "timed out awaiting initial turn/started; continuing best-effort"
            );
            return Ok(());
        }
        match tokio::time::timeout(deadline - now, rx.recv()).await {
            Ok(Ok(n)) => {
                if n.thread_id() == Some(thread_id)
                    && matches!(n, crate::codex_appserver::Notification::TurnStarted { .. })
                {
                    return Ok(());
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    skipped,
                    thread_id,
                    "shared worker initial lifecycle subscriber lagged"
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err(CalmError::CodexAppServer(format!(
                    "shared app-server notification channel closed before initial lifecycle for {thread_id}"
                )));
            }
            Err(_) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    thread_id,
                    "timed out awaiting initial turn/started; continuing best-effort"
                );
                return Ok(());
            }
        }
    }
}

/// `minted` is `Some` only right after this spawn minted `thread_id`: its resolved model then
/// replaces `resolved_model` (removed when codex named none). `None` keeps what that mint wrote.
async fn persist_shared_worker_runtime_fields(
    ctx: &SpawnCtx,
    card: &Card,
    runtime_id: &str,
    thread_id: &str,
    minted: Option<&MintedThread>,
    remote_uri: &str,
    active_turn_id: Option<&str>,
) -> Result<()> {
    let resolved_model_for_tx = minted.map(|minted| minted.resolved_model.clone());
    let card_id_for_tx = card.id.to_string();
    let runtime_id_for_tx = runtime_id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let active_turn_id_for_tx = active_turn_id.map(ToOwned::to_owned);
    let remote_uri_for_tx = remote_uri.to_string();
    write_in_tx_typed::<Card, _>(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut payload = card_payload_get_tx(tx, &card_id_for_tx).await?;
            let Some(map) = payload.as_object_mut() else {
                return Err(CalmError::Internal(format!(
                    "worker card {card_id_for_tx} payload is not a JSON object; cannot persist shared codex runtime fields"
                )));
            };
            map.insert("appserver_sock".into(), Value::String(remote_uri_for_tx));
            map.remove("appserver_pgid");
            match resolved_model_for_tx {
                Some(Some(model)) => {
                    map.insert("resolved_model".into(), Value::String(model));
                }
                Some(None) => {
                    map.remove("resolved_model");
                }
                None => {}
            }
            let updated = card_update_tx(
                tx,
                &card_id_for_tx,
                crate::model::CardPatch {
                    title: None,
                    kind: None,
                    sort: None,
                    payload: Some(payload),
                    deletable: None,
                },
            )
            .await?;
            let runtime = session_projection_by_id_tx(tx, &runtime_id_for_tx)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "worker runtime {runtime_id_for_tx} vanished before shared codex bind"
                    ))
                })?;
            let old_status = runtime.status;
            session_bind_attribution_tx(
                tx,
                &runtime.id,
                ThreadAttribution {
                    worker_session_id: runtime.id.clone(),
                    provider: AgentProvider::Codex,
                    thread_id: Some(thread_id_for_tx),
                    session_id: None,
                    active_turn_id: active_turn_id_for_tx,
                },
            )
            .await?;
            if old_status != WorkerSessionState::Running {
                session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
            }
            Ok(updated)
        })
    })
    .await?;
    Ok(())
}

pub(super) async fn verify_codex_worker_workspace(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    op: &Operation,
    output: &mut TxOutput,
) -> Result<()> {
    let card_id = output.output_string("card_id", "codex-worker")?;
    let track_id = output.output_string("track_id", "codex-worker")?;
    let runtime_id = output.output_string("runtime_id", "codex-worker")?;
    verify_worker_checkout(output, "codex-worker")?;

    let runtime_started_persisted = output
        .data
        .get("runtime_started_event_persisted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if runtime_started_persisted {
        return Ok(());
    }

    let scope = card_scope(
        ctx.repo.as_ref(),
        CardId::from(card_id.clone()),
        TrackId::from(track_id),
    )
    .await?;
    let mut checkpoint_output = output.clone();
    checkpoint_output.set_output_data(
        "runtime_started_event_persisted",
        json!(true),
        "codex-worker",
    )?;

    let card_id_for_tx = card_id.clone();
    let runtime_id_for_tx = runtime_id.clone();
    let op_for_tx = op.clone();
    let output_for_tx = checkpoint_output.clone();
    let expected_output_for_tx = output.clone();
    let write = WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
    write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::KernelDispatcher,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::RegisterPending {
                        entry_id: Some(card_id_for_tx.clone()),
                    },
                    &expected_output_for_tx,
                    &output_for_tx,
                )
                .await?;
                let events = vec![(
                    scope,
                    Event::WorkerSessionStarted {
                        worker_session_id: runtime_id_for_tx,
                        card_id: card_id_for_tx,
                        kind: WorkerSessionKind::CodexCard,
                        agent_provider: Some(AgentProvider::Codex),
                        status: WorkerSessionState::Starting,
                    },
                )];
                Ok(((), events))
            })
        },
    )
    .await?;
    *output = checkpoint_output;
    Ok(())
}
