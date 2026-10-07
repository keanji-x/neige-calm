use super::{
    config::AcpPlannerHost,
    session::{AcpPlannerSession, SessionParams},
};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::plugin_host::PluginHost;
use crate::thread_seals::ThreadSeals;
use calm_types::runtime::AgentProvider;
use std::sync::Arc;

#[derive(Clone)]
pub struct AcpPlannerWiring {
    pub host: Arc<AcpPlannerHost>,
    pub plugin: Arc<PluginHost>,
}
impl AcpPlannerWiring {
    pub async fn open_session(
        &self,
        provider: AgentProvider,
        repo: Arc<dyn Repo>,
        seals: Arc<ThreadSeals>,
        row: crate::claude_planner::wiring::ClaudePlannerRow<'_>,
    ) -> Result<Arc<AcpPlannerSession>> {
        let track = repo
            .track_get(row.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("ACP track".into()))?;
        let instructions = crate::operation::planner_harness_start_adapter::planner_instructions(
            repo.as_ref(),
            &self.plugin,
            row.track_id,
            row.card_id,
        )
        .await?;
        Ok(Arc::new(AcpPlannerSession::open(SessionParams {
            host: self.host.clone(),
            provider,
            repo,
            seals,
            worker_session_id: row.worker_session_id.into(),
            card_id: row.card_id.into(),
            track_id: row.track_id.into(),
            cwd: track.workspace.agent_cwd().into(),
            instructions,
        })))
    }
}
