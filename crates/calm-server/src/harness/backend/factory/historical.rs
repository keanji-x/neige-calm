//! Historical start effects, selected at the operation's existing lifecycle boundaries.
use super::PlannerFactory;
use crate::error::Result;
use crate::harness::{HarnessRegistry, HarnessSnapshot};
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::runtime::AgentProtocol;

/// Local identity never carries a credential: it is minted lazily at the first turn.
/// Native identity and the hash of the credential shipped with it are inseparable.
pub(crate) enum HistoricalIdentity {
    Local {
        thread_id: String,
    },
    Native {
        thread_id: String,
        mcp_token_hash: String,
    },
}

impl HistoricalIdentity {
    pub(crate) fn thread_id(&self) -> &str {
        match self {
            Self::Local { thread_id } | Self::Native { thread_id, .. } => thread_id,
        }
    }

    pub(crate) fn mcp_token_hash(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Native { mcp_token_hash, .. } => Some(mcp_token_hash),
        }
    }
}

impl PlannerFactory<'_> {
    /// Inside the transfer transaction, before the caller reads pending entries.
    pub(crate) async fn retire_inherited_checkpoint_tx(
        provider: &AgentProvider,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        predecessor: &str,
        snapshot: &mut HarnessSnapshot,
    ) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Acp => {
                crate::acp_planner::recovery::retire_before_transfer(tx, predecessor, snapshot)
                    .await
            }
            AgentProtocol::Claude | AgentProtocol::Codex => Ok(()),
        }
    }

    /// After registry shutdown and renewed admission, before the mint lock.
    pub(crate) async fn stop_historical_predecessor(
        &self,
        provider: &AgentProvider,
        card_id: &str,
        predecessor: &str,
    ) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Acp => {
                crate::acp_planner::lifecycle::stop_session(
                    self.acp,
                    predecessor,
                    crate::provider_execution::StopReason::Shutdown,
                )
                .await
            }
            AgentProtocol::Claude => {
                crate::claude_planner::lifecycle::stop_historical_predecessor(
                    self.claude,
                    card_id,
                    predecessor,
                )
                .await;
                Ok(())
            }
            AgentProtocol::Codex => Ok(()),
        }
    }

    /// Attribution stays in the checkpoint transaction. Local native-session identity
    /// survives rebinding, so the next spawn resumes rather than creates it again.
    pub(crate) async fn bind_historical_attribution_tx(
        provider: &AgentProvider,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker: &str,
        thread: &str,
    ) -> Result<()> {
        let session_id = match provider.protocol() {
            AgentProtocol::Codex => None,
            AgentProtocol::Claude | AgentProtocol::Acp => crate::db::sqlite::session_get_tx(
                tx,
                &calm_types::worker::WorkerSessionId(worker.to_string()),
            )
            .await?
            .and_then(|row| row.agent_session_id),
        };
        crate::db::sqlite::session_bind_attribution_tx(
            tx,
            &worker.to_string(),
            ThreadAttribution {
                worker_session_id: worker.to_string(),
                provider: provider.clone(),
                thread_id: Some(thread.to_string()),
                session_id,
                active_turn_id: None,
            },
        )
        .await?;
        Ok(())
    }

    /// The caller must adopt committed output first, even when this strict cleanup fails.
    pub(crate) async fn stop_displaced_historical(
        &self,
        provider: &AgentProvider,
        registry: &HarnessRegistry,
        displaced: Option<&str>,
    ) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Acp => {
                if let Some(displaced) = displaced {
                    registry.shutdown_and_remove(displaced).await?;
                    crate::acp_planner::lifecycle::stop_session(
                        self.acp,
                        displaced,
                        crate::provider_execution::StopReason::Shutdown,
                    )
                    .await?;
                }
                Ok(())
            }
            AgentProtocol::Claude | AgentProtocol::Codex => Ok(()),
        }
    }
}
