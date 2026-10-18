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

mod codex;
mod recovery;
pub(crate) use recovery::RecoveryTarget;

/// Preserve the existing operation vs HTTP refusal classes, with one readiness decision.
pub(crate) enum ReadinessFor {
    StartOperation,
    RestoreOrSend,
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

    /// Probe at the caller's existing decision point. Create admission is a separate contract.
    pub(crate) async fn ready(&self, provider: &AgentProvider, entry: ReadinessFor) -> Result<()> {
        match provider {
            AgentProvider::Codex => {
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
            AgentProvider::Claude => self.claude.check_ready().await,
            AgentProvider::OpenCode => self.acp.check_ready(provider).await,
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
        Ok(match row.binding.provider.clone() {
            AgentProvider::Codex => self.daemon()?.into(),
            AgentProvider::OpenCode => PlannerBackend(Arm::Acp(
                crate::acp_planner::wiring::open_session(self.acp, wiring, repo, seals, row)
                    .await?,
            )),
            AgentProvider::Claude => {
                PlannerBackend(Arm::Claude(wiring.open_session(repo, seals, row).await?))
            }
        })
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
        match provider {
            AgentProvider::OpenCode => crate::acp_planner::recovery::load_quiesced(
                repo,
                worker_session_id,
                card_id,
                track_id,
            )
            .await
            .map(Some),
            AgentProvider::Codex | AgentProvider::Claude => Ok(None),
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
        if managed && runtime.agent_provider == Some(AgentProvider::OpenCode) {
            return crate::planner_process::stop(&self.acp.instance, &runtime.id).await;
        }
        self.shutdown_codex_replay(runtime).await
    }
}
