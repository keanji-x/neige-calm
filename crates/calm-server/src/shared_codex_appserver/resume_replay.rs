//! A process owns its pending replay, authorization and cancellation progress.
use super::*;
use std::collections::HashMap;

mod authority;
use authority::cold_resume_authority_tx;

pub(crate) enum ConnectionReplay {
    Pending(ReplayPlan),
    Complete,
}

#[derive(Default)]
pub(crate) struct ReplayPlan {
    configs: HashMap<(String, String), PreparedColdResume>,
    excluded: HashSet<String>,
}
#[derive(Clone, PartialEq, Eq)]
struct ColdRuntime {
    id: String,
    thread: Option<String>,
}
#[derive(Clone, PartialEq, Eq)]
enum ColdAuthority {
    NoMcp(Option<ColdRuntime>),
    Mcp {
        runtime: ColdRuntime,
        role: CardRole,
    },
}
#[derive(Clone)]
struct PreparedColdResume {
    authority: ColdAuthority,
    config: ThreadConfig,
}

impl SharedCodexAppServer {
    pub(super) async fn resume_cached_threads(&self) {
        let _recovery = self.connection_recovery_serial.lock().await;
        let transition = self.transition_serial.lock().await;
        if let Err(error) = self.replay_cached_threads_locked(&transition).await {
            tracing::warn!(%error, "shared codex subscription restoration failed");
        }
    }

    async fn with_replay_plan<T>(
        &self,
        client: &Arc<CodexAppServer>,
        run: impl FnOnce(&mut ReplayPlan) -> T,
    ) -> Result<T> {
        let mut core = self.core.lock().await;
        match &mut core.state {
            SupervisorState::Running {
                client: current,
                replay: ConnectionReplay::Pending(plan),
                ..
            } if Arc::ptr_eq(current, client) => Ok(run(plan)),
            _ => Err(CalmError::Conflict("Codex replay source changed".into())),
        }
    }

    pub(super) async fn replay_cached_threads_locked(
        &self,
        transition: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<()> {
        let candidates = self.resume_candidates();
        for _ in 0..=candidates.len() {
            let Some(client) = self.reconnect_running_client_locked(transition).await? else {
                return Ok(());
            };
            {
                let core = self.core.lock().await;
                match &core.state {
                    SupervisorState::Running {
                        client: current,
                        replay,
                        ..
                    } if Arc::ptr_eq(current, &client) => match replay {
                        ConnectionReplay::Complete => return Ok(()),
                        ConnectionReplay::Pending(_) => {}
                    },
                    _ => return Err(CalmError::Conflict("Codex replay source changed".into())),
                }
            };
            for (thread_id, card_id) in &candidates {
                if self
                    .with_replay_plan(&client, |plan| plan.excluded.contains(thread_id))
                    .await?
                {
                    continue;
                }
                let _replay = self.resume_replay_serial.lock().await;
                if self.cached_card_for_thread(thread_id).as_deref() != Some(card_id.as_str()) {
                    continue;
                }
                match crate::conversation_inputs::has_canonical_storage(self.repo.as_ref(), card_id)
                    .await
                {
                    Ok(false) => {}
                    Ok(true) => continue, // Canonical recovery owns its exact source and admission.
                    Err(error) => {
                        tracing::warn!(%card_id, %error, "conversation storage contract unavailable; automatic resume held");
                        continue;
                    }
                }
                let key = (thread_id.clone(), card_id.clone());
                let previous = self
                    .with_replay_plan(&client, |plan| plan.configs.get(&key).cloned())
                    .await?;
                let authority = match self.replay_authority(thread_id, card_id).await {
                    Ok(Some(authority)) => authority,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%thread_id, %card_id, %error, "shared codex replay owner unavailable");
                        continue;
                    }
                };
                if previous
                    .as_ref()
                    .is_some_and(|prepared| prepared.authority != authority)
                {
                    continue;
                }
                let admission = NativePromptAdmission::new(
                    || self.require_replay_locator(thread_id, card_id),
                    || {
                        Box::pin(
                            self.revalidate_replay_owner(&client, thread_id, card_id, &authority),
                        )
                    },
                );
                // The provider reports whether this exact context is loaded. A
                // process restart/reconnect reason is not a thread-state witness.
                let native = match admission.finish(
                    client
                        .thread_read_gated(thread_id, false, admission.gate())
                        .await,
                ) {
                    Ok(native) => native,
                    Err(_) => {
                        self.with_replay_plan(&client, |plan| {
                            plan.excluded.insert(thread_id.clone())
                        })
                        .await?;
                        tracing::warn!(%thread_id, "cached native state unavailable; explicit recovery required");
                        if client.is_closed() {
                            break;
                        }
                        continue;
                    }
                };
                let cold = match native.thread.status {
                    provider::codex::ThreadStatus::NotLoaded => true,
                    provider::codex::ThreadStatus::Idle
                    | provider::codex::ThreadStatus::Active { .. }
                    | provider::codex::ThreadStatus::SystemError => false,
                };
                let config = if cold {
                    let Some(prepared) = self
                        .cold_resume_config(thread_id, card_id, previous, &authority)
                        .await
                    else {
                        continue;
                    };
                    let config = prepared.config.clone();
                    // Retain before the first native send, so cancellation cannot
                    // remint a token for a thread the daemon already loaded.
                    self.with_replay_plan(&client, |plan| plan.configs.insert(key, prepared))
                        .await?;
                    #[cfg(feature = "fixtures")]
                    crate::test_seams::pause_point("codex-replay-cold-prepared", card_id).await;
                    ResumeConfig::Reload(config)
                } else {
                    // An ambiguous earlier cold send retains its committed owner
                    // check, without reminting credentials or re-evaluating direnv.
                    if let Some(previous) = previous
                        && self
                            .cold_resume_config(thread_id, card_id, Some(previous), &authority)
                            .await
                            .is_none()
                    {
                        continue;
                    }
                    ResumeConfig::Retain
                };
                tracing::info!(%thread_id, %card_id, "resuming shared codex thread");
                self.resume_thread_typed(&client, thread_id, card_id, config, &admission)
                    .await;
                if client.is_closed() {
                    self.with_replay_plan(&client, |plan| plan.excluded.insert(thread_id.clone()))
                        .await?;
                    tracing::warn!(%thread_id, %card_id, "automatic resume closed the connection; leaving this thread for explicit recovery");
                    break;
                }
            }
            if !client.is_closed() {
                let mut core = self.core.lock().await;
                if let SupervisorState::Running {
                    client: current,
                    replay,
                    ..
                } = &mut core.state
                    && Arc::ptr_eq(current, &client)
                {
                    // Drop raw credentials and exclusions together at completion.
                    *replay = ConnectionReplay::Complete;
                    return Ok(());
                }
                return Err(CalmError::Conflict("Codex replay source changed".into()));
            }
            // Reattach earlier healthy threads too; exclusions survive cancellation.
        }
        Err(CalmError::CodexAppServer(
            "shared app-server subscription recovery is incomplete".into(),
        ))
    }

    async fn cold_resume_config(
        &self,
        thread_id: &str,
        card_id: &str,
        previous: Option<PreparedColdResume>,
        expected: &ColdAuthority,
    ) -> Option<PreparedColdResume> {
        let socket_path = self.paths.kernel_mcp_socket.clone();
        let result = write_in_tx_typed(self.repo.as_ref(), {
            let thread_id = thread_id.to_owned();
            let card_id = card_id.to_owned();
            let expected = expected.clone();
            move |tx| {
                Box::pin(async move {
                    let shape = crate::db::sqlite::card_execution_shape_tx(tx, &card_id).await?;
                    let Some(authority) =
                        cold_resume_authority_tx(tx, &thread_id, &card_id, &shape).await?
                    else {
                        return Ok(None);
                    };
                    if authority != expected {
                        return Ok(None);
                    }
                    let selection = if matches!(
                        &authority,
                        ColdAuthority::Mcp {
                            role: CardRole::Worker,
                            ..
                        }
                    ) {
                        match crate::worker_model_defaults::from_payload(
                            &shape.payload,
                            calm_types::worker_model::WorkerModelProvider::Codex,
                        )? {
                            Some(selection) => selection,
                            None => {
                                if crate::db::sqlite::card_is_worker_spawn_target_tx(tx, &card_id)
                                    .await?
                                {
                                    crate::worker_model_defaults::warn_legacy_selection(
                                        &card_id,
                                        calm_types::worker_model::WorkerModelProvider::Codex,
                                    );
                                }
                                TurnModelSelection::inherit()
                            }
                        }
                    } else {
                        TurnModelSelection::inherit()
                    };
                    // Re-run the owning authorization above on EVERY attempt, including
                    // active/failed state. A changed owner stays unloaded for explicit recovery.
                    if let Some(previous) = previous {
                        return Ok((previous.authority == authority).then_some(previous));
                    }
                    let config = match &authority {
                        ColdAuthority::NoMcp(_) => ThreadConfig::NoMcp,
                        ColdAuthority::Mcp { runtime, role } => {
                            let raw_token =
                                mint_and_persist_card_token(tx, &card_id, &runtime.id).await?;
                            ThreadConfig::McpShell {
                                role: *role,
                                socket_path,
                                raw_token,
                                selection,
                            }
                        }
                    };
                    Ok(Some(PreparedColdResume { authority, config }))
                })
            }
        })
        .await;
        match result {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(%thread_id, %card_id, %error, "shared codex authorization unavailable; leaving thread unloaded");
                None
            }
        }
    }

    async fn resume_thread_typed(
        &self,
        client: &CodexAppServer,
        thread_id: &str,
        card_id: &str,
        config: ResumeConfig,
        admission: &NativePromptAdmission<'_>,
    ) {
        let lowered = match config {
            ResumeConfig::Retain => Ok(None),
            ResumeConfig::Reload(config) => {
                self.workspace_thread_config(client, card_id, None, &config, admission)
                    .await
            }
        };
        let lowered = match lowered {
            Ok(lowered) => lowered,
            Err(error) => {
                tracing::warn!(%thread_id, %card_id, %error, "shared codex thread resume config unavailable");
                return;
            }
        };
        // This scope holds the process boundary. Bound the entire RPC, including
        // a partial send; cancellation uses the provider's synchronous abort guard.
        match tokio::time::timeout(client.request_timeout(), async {
            admission.finish(
                client
                    .thread_resume_with_config_gated(thread_id, lowered, admission.gate())
                    .await,
            )
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(%thread_id, %card_id, %error, "shared codex thread resume failed; leaving mapping intact")
            }
            Err(_) => {
                tracing::warn!(%thread_id, %card_id, "shared codex thread resume budget elapsed; leaving mapping intact")
            }
        }
    }
}
