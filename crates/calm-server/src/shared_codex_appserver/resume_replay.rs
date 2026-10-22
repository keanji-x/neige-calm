//! A process owns its pending replay, authorization and cancellation progress.
use super::*;
use std::collections::HashMap;

pub(crate) enum ConnectionReplay {
    Pending(ReplayPlan),
    Complete,
}

pub(crate) struct ReplayPlan {
    mode: ResumeMode,
    configs: HashMap<(String, String), PreparedColdResume>,
    excluded: HashSet<String>,
}
impl ReplayPlan {
    pub(super) fn new(mode: ResumeMode) -> Self {
        Self {
            mode,
            configs: HashMap::new(),
            excluded: HashSet::new(),
        }
    }
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
            let mode = {
                let core = self.core.lock().await;
                match &core.state {
                    SupervisorState::Running {
                        client: current,
                        replay,
                        ..
                    } if Arc::ptr_eq(current, &client) => match replay {
                        ConnectionReplay::Complete => return Ok(()),
                        ConnectionReplay::Pending(plan) => plan.mode,
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
                let config = match mode {
                    ResumeMode::HotTakeover => ThreadConfig::NoMcp,
                    ResumeMode::ColdRespawn => {
                        let key = (thread_id.clone(), card_id.clone());
                        let previous = self
                            .with_replay_plan(&client, |plan| plan.configs.get(&key).cloned())
                            .await?;
                        let Some(prepared) =
                            self.cold_resume_config(thread_id, card_id, previous).await
                        else {
                            continue;
                        };
                        let config = prepared.config.clone();
                        // Retain before the first native send, so cancellation cannot
                        // remint a token for a thread the daemon already loaded.
                        self.with_replay_plan(&client, |plan| plan.configs.insert(key, prepared))
                            .await?;
                        config
                    }
                };
                tracing::info!(%thread_id, %card_id, "resuming shared codex thread");
                Self::resume_thread_typed(&client, thread_id, card_id, config).await;
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
    ) -> Option<PreparedColdResume> {
        let socket_path = self.kernel_mcp_socket_path.clone();
        let result = write_in_tx_typed(self.repo.as_ref(), {
            let thread_id = thread_id.to_owned();
            let card_id = card_id.to_owned();
            move |tx| Box::pin(async move {
                let shape = crate::db::sqlite::card_execution_shape_tx(tx, &card_id).await?;
                let current = session_projection_active_for_card_tx(tx, &card_id).await?;
                let authority = if let Some(runtime) = current {
                    let stamp = ColdRuntime {id: runtime.id.clone(), thread: runtime.thread_id.clone()};
                    let binding = crate::harness::profile::PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload);
                    let no_mcp = match &binding {
                        Some(binding) => binding.profile.mcp_role().is_none() || binding.provider != AgentProvider::Codex,
                        None => shape.role == CardRole::Planner,
                    };
                    if runtime.thread_id.as_deref() != Some(thread_id.as_str()) || no_mcp {
                        ColdAuthority::NoMcp(Some(stamp))
                    } else {
                        ColdAuthority::Mcp {runtime: stamp, role: shape.role}
                    }
                } else {
                    let failed: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id WHERE c.id=?1 \
                         AND ws.state='failed' AND json_extract(ws.handle_state_json,'$.mode')=?2)"
                    ).bind(&card_id).bind(calm_types::harness::HARNESS_MODE).fetch_one(&mut **tx).await?;
                    if failed { return Ok(None); }
                    ColdAuthority::NoMcp(None)
                };
                // Re-run the owning authorization above on EVERY attempt, including
                // active/failed state. A changed owner stays unloaded for explicit recovery.
                if let Some(previous) = previous {
                    return Ok((previous.authority == authority).then_some(previous));
                }
                let config = match &authority {
                    ColdAuthority::NoMcp(_) => ThreadConfig::NoMcp,
                    ColdAuthority::Mcp {runtime, role} => {
                        let raw_token = mint_and_persist_card_token(tx, &card_id, &runtime.id).await?;
                        ThreadConfig::McpShell {role: *role, socket_path, raw_token}
                    }
                };
                Ok(Some(PreparedColdResume {authority, config}))
            })
        }).await;
        match result {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(%thread_id, %card_id, %error, "shared codex authorization unavailable; leaving thread unloaded");
                None
            }
        }
    }

    async fn resume_thread_typed(
        client: &CodexAppServer,
        thread_id: &str,
        card_id: &str,
        config: ThreadConfig,
    ) {
        let lowered = match config.to_wire_config() {
            Ok(lowered) => lowered,
            Err(error) => {
                tracing::warn!(%thread_id, %card_id, %error, "shared codex thread resume config unavailable");
                return;
            }
        };
        // This scope holds the process boundary. Bound the entire RPC, including
        // a partial send; cancellation uses the provider's synchronous abort guard.
        match tokio::time::timeout(
            client.request_timeout(),
            client.thread_resume_with_config(thread_id, lowered),
        )
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
