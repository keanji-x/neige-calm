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

/// Original carrier identity, constructed and consumed only within this recovery owner.
/// The daemon can read this opaque target but cannot construct or retarget it.
pub(crate) struct RecoveryTarget {
    thread_id: String,
    card_id: String,
}
impl RecoveryTarget {
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
    ) -> Result<()> {
        let target = RecoveryTarget::active(runtime)?;
        let daemon = self.daemon()?;
        daemon
            .with_exact_thread(&target, async |native| {
                Self::restore_active(native, repo, runtime).await
            })
            .await
    }
    pub(crate) async fn resume_system_error_conversation(
        &self,
        repo: &dyn crate::db::Repo,
        runtime: &WorkerSessionProjection,
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
                Self::restore_failed(native, repo, &runtime).await
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
    ) -> Result<()> {
        let thread_id = native.thread_id();
        let before = native.read_active().await?;
        if before.id.as_deref() != Some(thread_id) {
            return Err(CalmError::Conflict(
                "provider returned a different conversation".into(),
            ));
        }
        let cold = before.status.as_deref() == Some("notLoaded");
        if !cold
            && !matches!(
                before.status.as_deref(),
                Some("idle" | "systemError" | "active")
            )
        {
            return Err(CalmError::ServiceUnavailable(
                "Original conversation state is unknown; retry shortly".into(),
            ));
        }
        // A loaded thread still needs this connection's subscription restored.
        // Its existing credentials and running turn stay in place.
        let config = if cold {
            let socket_path = native.socket_path().to_owned();
            let expected = runtime.clone();
            let thread = thread_id.to_owned();
            write_in_tx_typed(repo, move |tx| {
                Box::pin(async move {
                    let current =
                        session_projection_active_for_card_tx(tx, &expected.card_id).await?;
                    if !current.as_ref().is_some_and(|current| {
                        current.id == expected.id
                            && crate::harness::effective_runtime_thread_id(current).as_deref()
                                == Some(thread.as_str())
                            && current.handle_state_json.eq(&expected.handle_state_json)
                    }) {
                        return Err(CalmError::Conflict(
                            "conversation changed during resume".into(),
                        ));
                    }
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
        let resumed = native.resume(config).await?;
        if resumed.id.as_deref() != Some(thread_id)
            || !matches!(
                resumed.status.as_deref(),
                Some("idle" | "systemError" | "active")
            )
            || (cold && resumed.status.as_deref() == Some("active"))
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
        Self::validate_recovery_carrier(repo, runtime, thread_id, &snapshot).await?;
        // thread/read and thread/resume share Codex's global exclusive thread queue, so after a
        // lost resume response this read waits for that resume to settle, across connections.
        let turn_id = snapshot.get("last_turn_id").and_then(Value::as_str);
        let before = native.read(turn_id).await?;
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
            // Persist before RPC: a lost response can leave a loaded thread.
            // Its token must already be known on retry, while Failed continues
            // to deny that token authority until the final restore commits.
            let raw = write_in_tx_typed(repo, move |tx| {
                Box::pin(async move {
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
        let resumed = native.resume(config).await?;
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
        let restored = write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
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
    ) -> Result<()> {
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let expected = snapshot.clone();
        let valid = write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
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
