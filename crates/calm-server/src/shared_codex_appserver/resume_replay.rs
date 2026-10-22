//! Automatic resume uses existing attribution and authorization; one broken history
//! may close a connection, but cannot hold every other subscription hostage.
use super::*;
use std::collections::HashMap;

impl SharedCodexAppServer {
    pub(super) async fn resume_cached_threads(&self, mode: ResumeMode) {
        let _recovery = self.connection_recovery_serial.lock().await;
        self.replay_cached_threads(mode).await;
    }

    // Caller holds connection_recovery_serial. Only a transport-failing candidate
    // is excluded for this pass; attribution remains available for explicit recovery.
    pub(super) async fn replay_cached_threads(&self, mode: ResumeMode) {
        let candidates = self.resume_candidates();
        let mut excluded = HashSet::new();
        let mut configs = HashMap::new();
        for _ in 0..=candidates.len() {
            let client = match self.reconnect_running_client().await {
                Ok(Some(client)) => client,
                Ok(None) => return,
                Err(error) => {
                    tracing::warn!(%error, "shared codex connection recovery failed");
                    return;
                }
            };
            for (thread_id, card_id) in &candidates {
                if excluded.contains(thread_id) {
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
                    Ok(true) => continue, // Canonical recovery owns its actual source and admission.
                    Err(error) => {
                        tracing::warn!(%card_id, %error, "conversation storage contract unavailable; automatic resume held");
                        continue;
                    }
                }
                if !configs.contains_key(thread_id) {
                    let config = if mode == ResumeMode::HotTakeover {
                        Some(ThreadConfig::NoMcp)
                    } else {
                        self.cold_resume_config(thread_id, card_id).await
                    };
                    configs.insert(thread_id.clone(), config);
                }
                let Some(config) = configs[thread_id].clone() else {
                    continue;
                };
                tracing::info!(%thread_id, %card_id, "resuming shared codex thread");
                Self::resume_thread_typed(&client, thread_id, card_id, config).await;
                if client.is_closed() {
                    excluded.insert(thread_id.clone());
                    tracing::warn!(%thread_id, %card_id, "automatic resume closed the connection; leaving this thread for explicit recovery");
                    break;
                }
            }
            if !client.is_closed() {
                let core = self.core.lock().await;
                if matches!(&core.state, SupervisorState::Running {client: current, ..} if Arc::ptr_eq(current, &client))
                {
                    self.connection_needs_replay.store(false, Ordering::SeqCst);
                    return;
                }
            }
            // Reattach earlier healthy threads too: their old subscription died.
            // Every failed pass excludes a new candidate, so this cannot loop on one history.
        }
    }

    async fn cold_resume_config(&self, thread_id: &str, card_id: &str) -> Option<ThreadConfig> {
        let (role, raw_token) = match write_in_tx_typed(self.repo.as_ref(), {
                let thread_id = thread_id.to_owned();
                let card_id = card_id.to_owned();
                move |tx| {
                    Box::pin(async move {
                        let shape=crate::db::sqlite::card_execution_shape_tx(tx,&card_id).await?;
                        let Some(runtime) =
                            session_projection_active_for_card_tx(tx, &card_id).await?
                        else {
                            // Decide inside the same transaction as the token
                            // choice: a systemError can arrive during replay.
                            let failed: bool = sqlx::query_scalar(
                                "SELECT EXISTS(SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id WHERE c.id=?1 \
                                    AND ws.state='failed' AND json_extract(ws.handle_state_json,'$.mode')=?2)"
                            ).bind(&card_id).bind(calm_types::harness::HARNESS_MODE).fetch_one(&mut **tx).await?;
                            return Ok(if failed { ColdResumeAuthorization::Skip } else { ColdResumeAuthorization::NoMcp });
                        };
                        if runtime.thread_id.as_deref() != Some(thread_id.as_str()) {
                            return Ok(ColdResumeAuthorization::NoMcp);
                        }
                        // One decoder for harness identity: a Planner card without a known Codex binding is not a
                        // harness card and gets no credential; a conversation profile without an MCP role neither.
                        let binding = crate::harness::profile::PlannerBinding::from_shape(&shape.kind,shape.role,&shape.payload);
                        let no_mcp = match &binding {
                            Some(binding) => binding.profile.mcp_role().is_none()
                                || binding.provider != crate::session_projection_repo::AgentProvider::Codex,
                            None => shape.role == CardRole::Planner,
                        };
                        if no_mcp {
                            return Ok(ColdResumeAuthorization::NoMcp);
                        }
                        let raw=mint_and_persist_card_token(tx, &card_id, &runtime.id).await?;
                        Ok(ColdResumeAuthorization::Token {role:shape.role,raw})
                    })
                }
            })
            .await
            {
                Ok(ColdResumeAuthorization::Token {role,raw}) => (role,raw),
                Ok(ColdResumeAuthorization::Skip) => return None,
                Ok(ColdResumeAuthorization::NoMcp) => {
                    return Some(ThreadConfig::NoMcp);
                }
                Err(e) => {
                    tracing::warn!(
                        target = "shared_codex_daemon::resume",
                        %thread_id,
                        %card_id,
                        error = %e,
                        "shared codex thread token refresh failed; leaving the thread unloaded"
                    );
                    return None;
                }
            };
        Some(ThreadConfig::McpShell {
            role,
            socket_path: self.kernel_mcp_socket_path.clone(),
            raw_token,
        })
    }

    async fn resume_thread_typed(
        client: &CodexAppServer,
        thread_id: &str,
        card_id: &str,
        config: ThreadConfig,
    ) {
        let lowered = match config.to_wire_config() {
            Ok(lowered) => lowered,
            Err(e) => {
                tracing::warn!(
                    target = "shared_codex_daemon::resume",
                    %thread_id,
                    %card_id,
                    error = %e,
                    "shared codex thread resume config unavailable; leaving mapping intact"
                );
                return;
            }
        };
        if let Err(e) = client.thread_resume_with_config(thread_id, lowered).await {
            tracing::warn!(
                target = "shared_codex_daemon::resume",
                %thread_id,
                %card_id,
                error = %e,
                "shared codex thread resume failed; leaving mapping intact"
            );
        }
    }
}
