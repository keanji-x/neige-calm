//! Exact-thread recovery. The caller owns the card/reset and track/delete
//! fences and has quiesced any failed harness before entering here.
use super::*;
use crate::db::sqlite::{session_resume_system_error_tx, session_system_error_recovery_matches_tx};
use crate::harness::profile::HarnessProfileExt;
use crate::session_projection_repo::WorkerSessionProjection;
use serde_json::Value;
#[cfg(feature = "fixtures")]
use serde_json::json;

/// A recovery caller owns its current-source/snapshot rule. The transport owner
/// invokes it in the same transaction as credential changes and after native gates.
#[async_trait::async_trait]
pub(crate) trait ActiveRecoveryAdmission: Send + Sync {
    async fn validate_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
}

struct HistoricalRecovery {
    runtime: WorkerSessionProjection,
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
}

#[async_trait::async_trait]
impl ActiveRecoveryAdmission for HistoricalRecovery {
    async fn validate_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        if let Some(source) = &self.source {
            source.require_card_tx(tx, &self.runtime.card_id).await?;
        }
        crate::conversation_inputs::require_historical_storage_tx(tx, &self.runtime.card_id)
            .await?;
        let current = session_projection_active_for_card_tx(tx, &self.runtime.card_id).await?;
        if !current.as_ref().is_some_and(|row| {
            row.id == self.runtime.id && row.handle_state_json == self.runtime.handle_state_json
        }) {
            return Err(CalmError::Conflict(
                "conversation changed during resume".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum LoadedTurnPolicy {
    PreserveHistoricalTurn,
    RequireIdle,
}

impl SharedCodexAppServer {
    /// Historical recovery retains its existing loaded-turn behavior. Canonical
    /// storage is refused by the owning admission, including after lock waits.
    pub(crate) async fn resume_active_conversation(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
    ) -> Result<()> {
        self.resume_historical_conversation(runtime, thread_id, None)
            .await
    }

    pub(crate) async fn resume_historical_conversation(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<()> {
        self.resume_active_core(
            runtime,
            thread_id,
            Arc::new(HistoricalRecovery {
                runtime: runtime.clone(),
                source,
            }),
            LoadedTurnPolicy::PreserveHistoricalTurn,
        )
        .await
    }

    /// A new Human input can restore an idle original thread, never retry an
    /// unresolved execution. This entry has no default or historical admission.
    pub(crate) async fn resume_active_conversation_admitted(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        admission: Arc<dyn ActiveRecoveryAdmission>,
    ) -> Result<()> {
        self.resume_active_core(runtime, thread_id, admission, LoadedTurnPolicy::RequireIdle)
            .await
    }

    async fn validate_active_recovery(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        admission: Arc<dyn ActiveRecoveryAdmission>,
    ) -> Result<()> {
        let runtime = runtime.clone();
        let thread = thread_id.to_owned();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                validate_active_recovery_tx(tx, &runtime, &thread, admission.as_ref()).await
            })
        })
        .await
    }

    async fn resume_active_core(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        admission: Arc<dyn ActiveRecoveryAdmission>,
        loaded: LoadedTurnPolicy,
    ) -> Result<()> {
        let _transition = self.transition_serial.lock().await;
        let _replay = self.resume_replay_serial.lock().await;
        if self.thread_seals.is_sealed(thread_id) {
            return Err(CalmError::Conflict("conversation is being deleted".into()));
        }
        let read = || {
            self.read_recovery_thread_admitted(thread_id, || {
                self.validate_active_recovery(runtime, thread_id, admission.clone())
            })
        };
        #[cfg(feature = "fixtures")]
        let before = if self.fake.is_some() && !self.thread_cache.contains_key(thread_id) {
            self.validate_active_recovery(runtime, thread_id, admission.clone())
                .await?;
            json!({"id":thread_id,"status":{"type":"notLoaded"}})
        } else {
            read().await?
        };
        #[cfg(not(feature = "fixtures"))]
        let before = read().await?;
        if before.get("id").and_then(Value::as_str) != Some(thread_id) {
            return Err(CalmError::Conflict(
                "provider returned a different conversation".into(),
            ));
        }
        let cold = match before.pointer("/status/type").and_then(Value::as_str) {
            Some("notLoaded") => true,
            Some("idle") => false,
            Some("systemError" | "active")
                if matches!(loaded, LoadedTurnPolicy::PreserveHistoricalTurn) =>
            {
                false
            }
            _ => {
                return Err(CalmError::ServiceUnavailable(
                    "Original conversation state is active or unknown; history retained".into(),
                ));
            }
        };
        let config = if cold {
            let socket_path = self.kernel_mcp_socket_path.clone();
            let expected = runtime.clone();
            let thread = thread_id.to_owned();
            let admission = admission.clone();
            write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    validate_active_recovery_tx(tx, &expected, &thread, admission.as_ref()).await?;
                    let shape =
                        crate::db::sqlite::card_execution_shape_tx(tx, &expected.card_id).await?;
                    let binding = crate::harness::profile::PlannerBinding::from_shape(
                        &shape.kind,
                        shape.role,
                        &shape.payload,
                    )
                    .filter(|binding| {
                        binding.provider == crate::session_projection_repo::AgentProvider::Codex
                    })
                    .ok_or_else(|| {
                        CalmError::Conflict("card is not a Codex harness conversation".into())
                    })?;
                    if binding.profile.mcp_role().is_none() {
                        return Ok(ThreadConfig::NoMcp);
                    }
                    let raw_token =
                        mint_and_persist_card_token(tx, &expected.card_id, &expected.id).await?;
                    Ok(ThreadConfig::McpShell {
                        role: shape.role,
                        socket_path,
                        raw_token,
                    })
                })
            })
            .await?
        } else {
            ThreadConfig::NoMcp
        };
        let resumed = self
            .resume_recovery_thread_admitted(thread_id, config, || {
                self.validate_active_recovery(runtime, thread_id, admission.clone())
            })
            .await?;
        let status = resumed.pointer("/status/type").and_then(Value::as_str);
        if resumed.get("id").and_then(Value::as_str) != Some(thread_id)
            || !matches!(status, Some("idle" | "systemError" | "active"))
            || (status == Some("active") && cold)
            || (matches!(loaded, LoadedTurnPolicy::RequireIdle) && status != Some("idle"))
        {
            return Err(CalmError::ServiceUnavailable(
                "Original conversation did not confirm resume; history retained".into(),
            ));
        }
        // This records the actual resume result, not admission for a new input.
        self.thread_cache
            .insert(thread_id.to_owned(), runtime.card_id.clone());
        self.hibernating_releases.remove(thread_id);
        Ok(())
    }
    pub(crate) async fn resume_system_error_conversation(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
    ) -> Result<Option<(i64, String)>> {
        self.resume_system_error_conversation_admitted(runtime, thread_id, None)
            .await
    }

    pub(crate) async fn resume_system_error_conversation_admitted(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<Option<(i64, String)>> {
        // Same order as daemon replacement -> cached-thread resume. A daemon
        // cannot turn a hot read into a cold resume while credentials are chosen.
        let _transition = self.transition_serial.lock().await;
        let _replay = self.resume_replay_serial.lock().await;
        if self.thread_seals.is_sealed(thread_id) {
            return Err(CalmError::Conflict("conversation is being deleted".into()));
        }
        let snapshot = runtime.handle_state_json.clone().ok_or_else(|| {
            CalmError::Conflict("conversation recovery snapshot is missing".into())
        })?;
        let role = self
            .repo
            .card_role_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("conversation role is missing".into()))?;
        let card = self
            .repo
            .card_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation no longer exists".into()))?;
        // This resume is a Codex app-server call: only a Codex binding may take it.
        let profile = crate::harness::profile::PlannerBinding::from_card(&card, role)
            .filter(|binding| {
                binding.provider == crate::session_projection_repo::AgentProvider::Codex
            })
            .ok_or_else(|| CalmError::Conflict("card is not a Codex harness conversation".into()))?
            .profile;
        self.validate_recovery_carrier(runtime, thread_id, &snapshot, source.clone())
            .await?;
        // thread/read and thread/resume share Codex's global exclusive thread queue, so after a
        // lost resume response this read waits for that resume to settle, across connections.
        let before = self
            .read_recovery_thread_admitted(thread_id, || {
                self.validate_recovery_carrier(runtime, thread_id, &snapshot, source.clone())
            })
            .await?;
        let cold = match before.get("status").and_then(|s| s.get("type")).and_then(Value::as_str) {
            Some("notLoaded") => true,
            Some("idle" | "systemError") => false,
            _ => return Err(CalmError::ServiceUnavailable(
                "The original conversation still has an active or unknown turn; retry after it settles. History is retained.".into())),
        };
        if before.get("id").and_then(Value::as_str) != Some(thread_id) {
            return Err(CalmError::Conflict(
                "provider returned a different conversation".into(),
            ));
        }
        let config = if cold && profile.mcp_role().is_some() {
            let card = runtime.card_id.clone();
            let id = runtime.id.clone();
            let thread = thread_id.to_owned();
            let expected = snapshot.clone();
            let admission = source.clone();
            // Persist before RPC: a lost response can leave a loaded thread.
            // Its token must already be known on retry, while Failed continues
            // to deny that token authority until the final restore commits.
            let raw = write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    if let Some(source) = admission {
                        source.require_card_tx(tx, &card).await?;
                    }
                    if !session_system_error_recovery_matches_tx(tx, &card, &id, &thread, &expected)
                        .await?
                    {
                        return Err(CalmError::Conflict(
                            "conversation changed during recovery".into(),
                        ));
                    }
                    mint_and_persist_card_token(tx, &card, &id).await
                })
            })
            .await?;
            ThreadConfig::McpShell {
                role,
                socket_path: self.kernel_mcp_socket_path.clone(),
                raw_token: raw,
            }
        } else {
            ThreadConfig::NoMcp
        };
        let resumed = self
            .resume_recovery_thread_admitted(thread_id, config, || {
                self.validate_recovery_carrier(runtime, thread_id, &snapshot, source.clone())
            })
            .await?;
        if resumed.get("id").and_then(Value::as_str) != Some(thread_id)
            || !matches!(
                resumed
                    .get("status")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str),
                Some("idle" | "systemError")
            )
        {
            return Err(CalmError::ServiceUnavailable(
                "The original conversation could not be confirmed idle; history is retained. Retry shortly.".into()));
        }
        // The old loop may have been quiesced before its queued completion.
        // Recover the exact last turn's terminal outcome from provider history,
        // without replaying its inputs or relying on a future notification.
        let outcome = self
            .retain_recovered_outcome(runtime, thread_id, &before)
            .await?;
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let admission = source;
        let restored = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                if let Some(source) = admission {
                    source.require_card_tx(tx, &card).await?;
                }
                Ok(session_resume_system_error_tx(tx, &card, &id, &thread, &snapshot).await?)
            })
        })
        .await?;
        if !restored {
            return Err(CalmError::Conflict(
                "conversation changed during recovery; message was not sent".into(),
            ));
        }
        self.thread_cache
            .insert(thread_id.to_owned(), runtime.card_id.clone());
        Ok(outcome)
    }

    async fn retain_recovered_outcome(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        thread: &Value,
    ) -> Result<Option<(i64, String)>> {
        let Some(turn_id) = runtime
            .handle_state_json
            .as_ref()
            .and_then(|s| s.get("last_turn_id"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(turn) = thread
            .get("turns")
            .and_then(Value::as_array)
            .and_then(|turns| {
                turns.iter().find(|turn| {
                    turn.get("id").and_then(Value::as_str) == Some(turn_id)
                        && matches!(
                            turn.get("status").and_then(Value::as_str),
                            Some("completed" | "failed" | "interrupted")
                        )
                })
            })
        else {
            return Ok(None);
        };
        let card = self
            .repo
            .card_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation disappeared".into()))?;
        let id = crate::harness::turn_outcome::record(
            self.repo.as_ref(),
            &runtime.id,
            &runtime.card_id,
            card.track_id.as_str(),
            thread_id,
            turn_id,
            turn,
            crate::harness::turn_outcome::Confirmation::ProviderTerminal,
        )
        .await?;
        Ok(Some((id, turn_id.to_owned())))
    }

    async fn validate_recovery_carrier(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        snapshot: &Value,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<()> {
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let expected = snapshot.clone();
        let valid = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                if let Some(source) = source {
                    source.require_card_tx(tx, &card).await?;
                }
                Ok(
                    session_system_error_recovery_matches_tx(tx, &card, &id, &thread, &expected)
                        .await?,
                )
            })
        })
        .await?;
        if valid {
            Ok(())
        } else {
            Err(CalmError::Conflict(
                "conversation is no longer eligible for recovery; history is retained".into(),
            ))
        }
    }

    async fn read_recovery_thread_admitted<F, Fut>(
        &self,
        thread_id: &str,
        admit: F,
    ) -> Result<Value>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            admit().await?;
            let status = if self.active_turn_id_for_thread(thread_id).is_some() {
                "active"
            } else {
                "systemError"
            };
            return Ok(json!({"id":thread_id,"status":{"type":status}}));
        }
        let client = self.connected_client().await?;
        admit().await?;
        Ok(client.thread_read_full(thread_id).await?.thread)
    }

    async fn resume_recovery_thread_admitted<F, Fut>(
        &self,
        thread_id: &str,
        config: ThreadConfig,
        admit: F,
    ) -> Result<Value>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            admit().await?;
            if fake.fail_thread_resume.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "forced thread/resume failure".into(),
                ));
            }
            fake.resumed_threads
                .lock()
                .expect("fake resumed threads mutex")
                .push((
                    thread_id.to_owned(),
                    matches!(config, ThreadConfig::McpShell { .. }),
                ));
            return Ok(json!({"id":thread_id,"status":{"type":"idle"}}));
        }
        let config = config.to_wire_config()?;
        let client = self.connected_client().await?;
        admit().await?;
        Ok(client
            .thread_resume_with_config(thread_id, config)
            .await?
            .thread)
    }

    #[cfg(feature = "fixtures")]
    pub fn resumed_threads_for_test(&self) -> Vec<(String, bool)> {
        self.fake
            .as_ref()
            .expect("fake daemon")
            .resumed_threads
            .lock()
            .unwrap()
            .clone()
    }
    #[cfg(feature = "fixtures")]
    pub fn fail_thread_resume_for_test(&self) {
        self.fake
            .as_ref()
            .expect("fake daemon")
            .fail_thread_resume
            .store(true, Ordering::SeqCst);
    }
}

async fn validate_active_recovery_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    expected: &WorkerSessionProjection,
    thread: &str,
    admission: &dyn ActiveRecoveryAdmission,
) -> Result<()> {
    let current = session_projection_active_for_card_tx(tx, &expected.card_id).await?;
    if !current.as_ref().is_some_and(|row| {
        row.id == expected.id
            && crate::harness::effective_runtime_thread_id(row).as_deref() == Some(thread)
    }) {
        return Err(CalmError::Conflict(
            "conversation changed during resume".into(),
        ));
    }
    admission.validate_tx(tx).await
}
