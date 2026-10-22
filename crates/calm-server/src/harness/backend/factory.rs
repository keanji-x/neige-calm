//! Concrete server construction and readiness. Borrowed wiring only; no cached readiness,
//! scheduling state, credential, process or native work is created by `new`.
use std::sync::{Arc, Weak};

use super::{Arm, PlannerBackend};
use crate::acp_planner::config::AcpPlannerHost;
use crate::claude_planner::config::ClaudePlannerHost;
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::wiring::{PlannerRow, PlannerWiring};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::thread_seals::ThreadSeals;
use calm_types::runtime::AgentProtocol;

mod catalog_readiness;
pub(crate) use catalog_readiness::CatalogReadiness;
mod codex;
mod historical;
mod recovery;
pub(crate) use recovery::{ActiveRecoveryAdmission, RecoveryTarget};

/// Preserve the existing operation vs HTTP refusal classes, with one readiness decision.
pub(crate) enum ReadinessFor {
    StartOperation,
    RestoreOrSend,
}

/// Identity strategy at the existing retained operation checkpoint. Querying it
/// allocates no identity and starts no process. Local bindings are Neige-owned.
pub(crate) enum RetainedIdentityPreparation {
    NativeThread,
    LocalSession,
}

pub(crate) struct PlannerFactory<'a> {
    daemon: Weak<SharedCodexAppServer>,
    claude: &'a Arc<ClaudePlannerHost>,
    acp: &'a Arc<AcpPlannerHost>,
}

impl<'a> PlannerFactory<'a> {
    pub(crate) fn new(
        daemon: Weak<SharedCodexAppServer>,
        claude: &'a Arc<ClaudePlannerHost>,
        acp: &'a Arc<AcpPlannerHost>,
    ) -> Self {
        Self {
            daemon,
            claude,
            acp,
        }
    }

    /// Pure canonical support query: unsupported profiles refuse independently of readiness.
    pub(crate) fn retained_identity(
        provider: &AgentProvider,
    ) -> Result<RetainedIdentityPreparation> {
        match provider.protocol() {
            AgentProtocol::Codex => Ok(RetainedIdentityPreparation::NativeThread),
            AgentProtocol::Claude => Ok(RetainedIdentityPreparation::LocalSession),
            AgentProtocol::Acp => {
                if !crate::acp_planner::wiring::canonical_planner_inputs(provider) {
                    return Err(CalmError::Conflict(
                        "ACP registration does not support canonical Planner inputs".into(),
                    ));
                }
                Ok(RetainedIdentityPreparation::LocalSession)
            }
        }
    }

    /// Codex recovery must observe an idle native thread. Local recovery can
    /// reopen control for a held checkpoint after exact predecessor quiescence.
    pub(crate) fn canonical_recovery_requires_idle(provider: &AgentProvider) -> bool {
        match provider.protocol() {
            AgentProtocol::Codex => true,
            AgentProtocol::Claude | AgentProtocol::Acp => false,
        }
    }

    /// Probe at the caller's existing decision point. Create admission is a separate contract.
    pub(crate) async fn ready(&self, provider: &AgentProvider, entry: ReadinessFor) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Codex => {
                let daemon = self.daemon()?;
                if daemon.is_running() {
                    return Ok(());
                }
                Err(match entry {
                    ReadinessFor::StartOperation => daemon.not_running_error(),
                    ReadinessFor::RestoreOrSend => {
                        CalmError::ServiceUnavailable(daemon.not_running_message())
                    }
                })
            }
            AgentProtocol::Claude => self.claude.check_ready().await,
            AgentProtocol::Acp => self.acp.check_ready(provider).await,
        }
    }

    /// Local canonical activation validates configuration without probing a
    /// native process. Native negotiation belongs to the installed first issue.
    pub(crate) async fn ready_retained(&self, provider: &AgentProvider) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Acp => self.acp.configured(provider).map(|_| ()),
            AgentProtocol::Codex | AgentProtocol::Claude => {
                self.ready(provider, ReadinessFor::StartOperation).await
            }
        }
    }

    fn daemon(&self) -> Result<Arc<SharedCodexAppServer>> {
        self.daemon
            .upgrade()
            .ok_or_else(|| CalmError::ServiceUnavailable("application daemon stopped".into()))
    }

    /// Open only the existing control object. Local sessions mint/spawn on the installed first
    /// turn, while Codex preparation remains at the operation's original journal checkpoint.
    pub(crate) async fn open(
        &self,
        seals: Arc<ThreadSeals>,
        wiring: &PlannerWiring,
        repo: Arc<dyn Repo>,
        row: PlannerRow<'_>,
    ) -> Result<PlannerBackend> {
        Ok(match row.binding.provider.protocol() {
            AgentProtocol::Codex => PlannerBackend::codex(
                self.daemon()?,
                row.card_id,
                row.track_id,
                row.worker_session_id,
            )?,
            AgentProtocol::Acp => PlannerBackend(Arm::Acp(
                crate::acp_planner::wiring::open_session(self.acp, wiring, repo, seals, row)
                    .await?,
            )),
            AgentProtocol::Claude => PlannerBackend(Arm::Claude(
                crate::claude_planner::wiring::open_session(self.claude, wiring, repo, seals, row)
                    .await?,
            )),
        })
    }

    /// Conventional historical catch-up must not run before ACP receipt retirement.
    /// ACP restores its quiesced checkpoint first; the harness then folds later events.
    /// Planner role admission and queue/watermark updates remain in the harness.
    pub(crate) fn historical_replay_eligible(provider: &AgentProvider) -> bool {
        match provider.protocol() {
            AgentProtocol::Codex | AgentProtocol::Claude => true,
            AgentProtocol::Acp => false,
        }
    }

    /// Restore-only policy, at the original post-reservation/pre-open point. Operation replay
    /// deliberately uses prepare_checkpoint alone.
    pub(crate) async fn prepare_restore(
        &self,
        provider: &AgentProvider,
        repo: &dyn Repo,
        runtime: &crate::session_projection_repo::WorkerSessionProjection,
        track_id: &str,
        snapshot: &crate::harness::HarnessSnapshot,
    ) -> Result<()> {
        match provider.protocol() {
            AgentProtocol::Claude => {
                crate::claude_planner::recovery::record_interrupted_turn(
                    repo, runtime, track_id, snapshot,
                )
                .await
            }
            AgentProtocol::Codex | AgentProtocol::Acp => Ok(()),
        }
    }

    /// Call only after the caller's predecessor reservation has settled. ACP owns full
    /// receipt/checkpoint recovery; other providers retain their entry-specific fallback.
    pub(crate) async fn prepare_checkpoint(
        &self,
        provider: &AgentProvider,
        repo: &dyn Repo,
        worker_session_id: &str,
        card_id: &str,
        track_id: &str,
    ) -> Result<Option<crate::harness::HarnessSnapshot>> {
        match provider.protocol() {
            AgentProtocol::Acp => crate::acp_planner::recovery::load_quiesced(
                repo,
                worker_session_id,
                card_id,
                track_id,
            )
            .await
            .map(Some),
            AgentProtocol::Codex | AgentProtocol::Claude => Ok(None),
        }
    }

    /// Registry-miss cleanup uses persisted ownership facts, never a reopened session.
    pub(crate) async fn shutdown_replay(
        &self,
        runtime: &crate::session_projection_repo::WorkerSessionProjection,
    ) -> Result<()> {
        use crate::session_projection_repo::WorkerSessionKind;
        let managed = matches!(
            runtime.kind,
            WorkerSessionKind::SharedPlanner | WorkerSessionKind::ManagedAssistant
        );
        let protocol = runtime.agent_provider.as_ref().map(AgentProvider::protocol);
        match protocol {
            Some(AgentProtocol::Claude) if managed => {
                crate::claude_planner::lifecycle::shutdown_replay(self.claude, &runtime.id).await;
                Ok(())
            }
            Some(AgentProtocol::Acp) if managed => {
                crate::planner_process::stop(&self.acp.instance, &runtime.id).await
            }
            Some(AgentProtocol::Codex) | None => self.shutdown_codex_replay(runtime).await,
            Some(AgentProtocol::Claude | AgentProtocol::Acp) => {
                self.shutdown_codex_replay(runtime).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_historical_replay_waits_for_acp_receipt_recovery() {
        assert!(PlannerFactory::historical_replay_eligible(
            &AgentProvider::Codex
        ));
        assert!(PlannerFactory::historical_replay_eligible(
            &AgentProvider::Claude
        ));
        assert!(!PlannerFactory::historical_replay_eligible(
            &AgentProvider::OpenCode
        ));
        assert!(!PlannerFactory::historical_replay_eligible(
            &AgentProvider::Grok
        ));
    }
}
