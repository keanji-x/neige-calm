//! Concrete server construction and readiness. Borrowed wiring only; no cached readiness,
//! scheduling state, credential, process or native work is created by `new`.
use std::sync::{Arc, Weak};

use super::{Arm, PlannerBackend};
use crate::acp_planner::config::AcpPlannerHost;
use crate::claude_planner::config::ClaudePlannerHost;
use crate::claude_planner::wiring::{ClaudePlannerRow, ClaudePlannerWiring};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::thread_seals::ThreadSeals;

mod codex;
pub(crate) mod recovery;

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
        claude: &ClaudePlannerWiring,
        repo: Arc<dyn Repo>,
        row: ClaudePlannerRow<'_>,
    ) -> Result<PlannerBackend> {
        Ok(match row.binding.provider.clone() {
            AgentProvider::Codex => self.daemon()?.into(),
            AgentProvider::OpenCode => {
                let acp = crate::acp_planner::wiring::AcpPlannerWiring {
                    host: claude.acp.clone(),
                    plugin: claude.plugin.clone(),
                };
                PlannerBackend(Arm::Acp(
                    acp.open_session(AgentProvider::OpenCode, repo, seals, row)
                        .await?,
                ))
            }
            AgentProvider::Claude => {
                PlannerBackend(Arm::Claude(claude.open_session(repo, seals, row).await?))
            }
        })
    }
}
