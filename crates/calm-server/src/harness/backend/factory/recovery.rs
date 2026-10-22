//! Host exact-thread restoration. Callers retain card/reset and track/delete fences.
use super::PlannerFactory;
use crate::db::sqlite::{
    session_projection_active_for_card_tx, session_resume_system_error_tx,
    session_system_error_recovery_matches_tx,
};
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, Result};
use crate::harness::profile::HarnessProfileExt;
use crate::mcp_server::wiring::mint_and_persist_card_token;
use crate::session_projection_repo::WorkerSessionProjection;
use crate::shared_codex_appserver::ThreadConfig;
use serde_json::Value;
use std::sync::Arc;
/// A recovery caller owns its current-source/snapshot rule. The transport owner
/// invokes it in the same transaction as credential changes and after native gates.
#[async_trait::async_trait]
pub(crate) trait ActiveRecoveryAdmission: Send + Sync {
    fn check_current(&self) -> Result<()>;
    async fn validate_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
}

struct HistoricalRecovery {
    runtime: WorkerSessionProjection,
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
}

#[async_trait::async_trait]
impl ActiveRecoveryAdmission for HistoricalRecovery {
    fn check_current(&self) -> Result<()> {
        match &self.source {
            Some(source) => source.check_current(),
            None => Ok(()), // Existing historical kernel recovery.
        }
    }

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

/// Original carrier identity, constructed and consumed only within this recovery owner.
/// The daemon can read this opaque target but cannot construct or retarget it.
pub(crate) struct RecoveryTarget {
    thread_id: String,
    card_id: String,
}
impl RecoveryTarget {
    pub(in crate::harness::backend) fn for_settlement(card: &str, thread: &str) -> Result<Self> {
        if card.is_empty() || thread.is_empty() {
            return Err(CalmError::Conflict(
                "settlement requires an exact conversation".into(),
            ));
        }
        Ok(Self {
            card_id: card.into(),
            thread_id: thread.into(),
        })
    }

    fn active(runtime: &WorkerSessionProjection) -> Result<Self> {
        Self::from_carrier(runtime, "conversation thread is missing")
    }
    fn failed(runtime: &WorkerSessionProjection) -> Result<Self> {
        Self::from_carrier(
            runtime,
            "original conversation thread is missing; history is retained",
        )
    }
    fn from_carrier(runtime: &WorkerSessionProjection, missing: &str) -> Result<Self> {
        Ok(Self {
            thread_id: crate::harness::effective_runtime_thread_id(runtime)
                .ok_or_else(|| CalmError::Conflict(missing.into()))?,
            card_id: runtime.card_id.clone(),
        })
    }
    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub(crate) fn card_id(&self) -> &str {
        &self.card_id
    }
}

impl PlannerFactory<'_> {
    pub(crate) async fn resume_active_conversation(
        &self,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<()> {
        self.resume_active_conversation_admitted(
            repo,
            runtime,
            Arc::new(HistoricalRecovery {
                runtime: runtime.clone(),
                source,
            }),
            LoadedTurnPolicy::PreserveHistoricalTurn,
        )
        .await
    }

    pub(crate) async fn resume_canonical_conversation(
        &self,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        admission: Arc<dyn ActiveRecoveryAdmission>,
    ) -> Result<()> {
        let provider = runtime
            .agent_provider
            .as_ref()
            .ok_or_else(|| CalmError::Conflict("canonical recovery provider is missing".into()))?;
        match provider.protocol() {
            calm_types::runtime::AgentProtocol::Codex => {
                self.resume_active_conversation_admitted(
                    repo,
                    runtime,
                    admission,
                    LoadedTurnPolicy::RequireIdle,
                )
                .await
            }
            calm_types::runtime::AgentProtocol::Claude => {
                let target = RecoveryTarget::active(runtime)?;
                validate_active_recovery(repo, runtime, target.thread_id(), admission.clone())
                    .await?;
                crate::claude_planner::recovery::quiesce_canonical(self.claude, &runtime.id)
                    .await?;
                validate_active_recovery(repo, runtime, target.thread_id(), admission).await
            }
            calm_types::runtime::AgentProtocol::Acp => {
                if !crate::acp_planner::wiring::canonical_planner_inputs(provider) {
                    return Err(CalmError::Conflict(
                        "ACP registration does not support canonical Planner inputs".into(),
                    ));
                }
                let target = RecoveryTarget::active(runtime)?;
                // ACP receipt/cleanup recovery runs only after the retained
                // install reservation. Validation alone cannot stop a winner.
                validate_active_recovery(repo, runtime, target.thread_id(), admission).await
            }
        }
    }

    async fn resume_active_conversation_admitted(
        &self,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        admission: Arc<dyn ActiveRecoveryAdmission>,
        loaded: LoadedTurnPolicy,
    ) -> Result<()> {
        let target = RecoveryTarget::active(runtime)?;
        self.daemon()?
            .with_exact_thread(&target, async |native| {
                Self::restore_active(native, repo, runtime, admission, loaded).await
            })
            .await
    }
    pub(crate) async fn resume_system_error_conversation(
        &self,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
        settle: impl AsyncFnOnce() -> Result<()>,
    ) -> Result<Option<(i64, String)>> {
        // Refuse a missing original thread before quiescing the route-owned harness.
        let target = RecoveryTarget::failed(runtime)?;
        settle().await?;
        // Quiescence may have restored dropped steers. Refetch the same session's
        // settled queue without changing its original native target.
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(crate::test_seams::PLANNER_RECOVERY_REFETCH, &runtime.id)
            .await;
        let runtime = repo
            .session_projection_by_id(&runtime.id)
            .await?
            .ok_or_else(|| CalmError::Conflict("conversation changed during recovery".into()))?;
        let daemon = self.daemon()?;
        daemon
            .with_exact_thread(&target, async |native| {
                Self::restore_failed(native, repo, &runtime, source).await
            })
            .await
    }
    /// Active carrier restoration uses the same exact-thread read/resume RPCs as
    /// system-error recovery. A cold thread receives this carrier's MCP token;
    /// a loaded thread keeps its existing credentials.
    async fn restore_active(
        native: crate::shared_codex_appserver::exact_thread::RecoveryThread<'_>,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        admission: Arc<dyn ActiveRecoveryAdmission>,
        loaded: LoadedTurnPolicy,
    ) -> Result<()> {
        let thread_id = native.thread_id();
        let admit = crate::harness::native_admission::NativePromptAdmission::new(
            || admission.check_current(),
            || -> futures_util::future::BoxFuture<'_, Result<()>> {
                Box::pin(validate_active_recovery(
                    repo,
                    runtime,
                    thread_id,
                    admission.clone(),
                ))
            },
        );
        let before = native.read_active(&admit).await?;
        if before.id.as_deref() != Some(thread_id) {
            return Err(CalmError::Conflict(
                "provider returned a different conversation".into(),
            ));
        }
        let cold = before.status.as_deref() == Some("notLoaded");
        if !cold
            && !matches!(before.status.as_deref(), Some("idle"))
            && !(matches!(loaded, LoadedTurnPolicy::PreserveHistoricalTurn)
                && matches!(before.status.as_deref(), Some("systemError" | "active")))
        {
            return Err(CalmError::ServiceUnavailable(
                "Original conversation state is active or unknown; history retained".into(),
            ));
        }
        // A loaded thread still needs this connection's subscription restored.
        // Its existing credentials and running turn stay in place.
        let config = if cold {
            let socket_path = native.socket_path().to_owned();
            let expected = runtime.clone();
            let thread = thread_id.to_owned();
            let admission = admission.clone();
            write_in_tx_typed(repo, move |tx| {
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
        let resumed = native.resume(config, &admit).await?;
        if resumed.id.as_deref() != Some(thread_id)
            || !matches!(
                resumed.status.as_deref(),
                Some("idle" | "systemError" | "active")
            )
            || (cold && resumed.status.as_deref() == Some("active"))
            || (matches!(loaded, LoadedTurnPolicy::RequireIdle)
                && resumed.status.as_deref() != Some("idle"))
        {
            return Err(CalmError::ServiceUnavailable(
                "Original conversation did not confirm resume; history retained".into(),
            ));
        }
        native.retain_subscription();
        native.clear_hibernating_release();
        Ok(())
    }
    async fn restore_failed(
        native: crate::shared_codex_appserver::exact_thread::RecoveryThread<'_>,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<Option<(i64, String)>> {
        // Same order as daemon replacement -> cached-thread resume. A daemon
        // cannot turn a hot read into a cold resume while credentials are chosen.
        let thread_id = native.thread_id();
        let snapshot = runtime.handle_state_json.clone().ok_or_else(|| {
            CalmError::Conflict("conversation recovery snapshot is missing".into())
        })?;
        let role = repo
            .card_role_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("conversation role is missing".into()))?;
        let card = repo
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
        let admit = crate::harness::native_admission::NativePromptAdmission::new(
            || match &source {
                Some(source) => source.check_current(),
                None => Ok(()), // Historical kernel recovery.
            },
            || -> futures_util::future::BoxFuture<'_, Result<()>> {
                Box::pin(Self::validate_recovery_carrier(
                    repo,
                    runtime,
                    thread_id,
                    &snapshot,
                    source.clone(),
                ))
            },
        );
        // thread/read and thread/resume share Codex's global exclusive thread queue, so after a
        // lost resume response this read waits for that resume to settle, across connections.
        let turn_id = snapshot.get("last_turn_id").and_then(Value::as_str);
        let before = native.read(turn_id, &admit).await?;
        let cold = before.state.status.as_deref() == Some("notLoaded");
        if !cold && !matches!(before.state.status.as_deref(), Some("idle" | "systemError")) {
            return Err(CalmError::ServiceUnavailable(
                "The original conversation still has an active or unknown turn; retry after it settles. History is retained.".into()));
        }
        if before.state.id.as_deref() != Some(thread_id) {
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
            let raw = write_in_tx_typed(repo, move |tx| {
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
                socket_path: native.socket_path().to_owned(),
                raw_token: raw,
            }
        } else {
            ThreadConfig::NoMcp
        };
        let resumed = native.resume(config, &admit).await?;
        if resumed.id.as_deref() != Some(thread_id)
            || !matches!(resumed.status.as_deref(), Some("idle" | "systemError"))
        {
            return Err(CalmError::ServiceUnavailable(
                "The original conversation could not be confirmed idle; history is retained. Retry shortly.".into()));
        }
        // The old loop may have been quiesced before its queued completion.
        // Recover the exact last turn's terminal outcome from provider history,
        // without replaying its inputs or relying on a future notification.
        let outcome = Self::retain_recovered_outcome(
            repo,
            runtime,
            thread_id,
            turn_id,
            before.terminal.as_ref(),
        )
        .await?;
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        drop(admit);
        let admission = source;
        let restored = write_in_tx_typed(repo, move |tx| {
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
        native.retain_subscription();
        Ok(outcome)
    }

    async fn retain_recovered_outcome(
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        turn_id: Option<&str>,
        terminal: Option<&Value>,
    ) -> Result<Option<(i64, String)>> {
        let (Some(turn_id), Some(turn)) = (turn_id, terminal) else {
            return Ok(None);
        };
        let card = repo
            .card_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation disappeared".into()))?;
        let id = crate::harness::turn_outcome::record(
            repo,
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
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        snapshot: &Value,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Result<()> {
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let expected = snapshot.clone();
        let valid = write_in_tx_typed(repo, move |tx| {
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
}

async fn validate_active_recovery_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    expected: &WorkerSessionProjection,
    thread: &str,
    admission: &dyn ActiveRecoveryAdmission,
) -> Result<()> {
    let owner = calm_truth::decision_gate::WriteTx::read_worker_session(
        tx,
        &calm_types::worker::WorkerSessionId(expected.id.clone()),
    )
    .await?
    .ok_or_else(|| CalmError::Conflict("conversation authority changed during resume".into()))?;
    if !owner.state.is_active_authority() {
        return Err(CalmError::Conflict(
            "conversation authority retired during resume".into(),
        ));
    }
    let current = session_projection_active_for_card_tx(tx, &expected.card_id).await?;
    if !current.as_ref().is_some_and(|row| {
        row.id == expected.id
            && crate::harness::effective_runtime_thread_id(row).as_deref() == Some(thread)
            && row.session_id == expected.session_id
            && row.agent_provider == expected.agent_provider
    }) {
        return Err(CalmError::Conflict(
            "conversation changed during resume".into(),
        ));
    }
    admission.validate_tx(tx).await?;
    admission.check_current()
}

async fn validate_active_recovery(
    repo: &dyn crate::db::Repo,
    runtime: &WorkerSessionProjection,
    thread: &str,
    admission: Arc<dyn ActiveRecoveryAdmission>,
) -> Result<()> {
    let runtime = runtime.clone();
    let thread = thread.to_owned();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            validate_active_recovery_tx(tx, &runtime, &thread, admission.as_ref()).await
        })
    })
    .await
}

impl crate::harness::backend::PlannerBackend {
    /// The caller owns the retained install reservation and has no live predecessor.
    pub(crate) async fn recover_retained(&self) -> Result<()> {
        match &self.0 {
            crate::harness::backend::Arm::Acp(session) => session.recover_retained().await,
            crate::harness::backend::Arm::Claude(_) | crate::harness::backend::Arm::Codex(..) => {
                Ok(())
            }
        }
    }
}
