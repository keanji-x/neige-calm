//! What the server needs to open a Claude Planner session for a Planner row, shared by the start
//! adapter and every recovery path (design #1791 §4.4, §5.1 item 2, §5.2).

use std::sync::Arc;

#[cfg(any(test, feature = "fixtures"))]
use super::config::ClaudePlannerHost;
use super::session::{ClaudePlannerSession, ClaudePlannerSessionParams};
use crate::db::Repo;
use crate::error::{CalmError, Result};
#[cfg(any(test, feature = "fixtures"))]
use crate::plugin_host::PluginHost;
use crate::thread_seals::ThreadSeals;

/// Appended to the Planner rendering for Claude only (§5.2).
const CLAUDE_FRAGMENT: &str = include_str!("../../prompts/claude-planner/long-lived-processes.md");

pub use crate::harness::wiring::{
    PlannerRow as ClaudePlannerRow, PlannerWiring as ClaudePlannerWiring,
};

impl ClaudePlannerWiring {
    /// Render the instructions once and open the session. The host's config is not required here:
    /// a session opened without it refuses every turn with a message naming the flag.
    pub async fn open_session(
        &self,
        repo: Arc<dyn Repo>,
        seals: Arc<ThreadSeals>,
        row: ClaudePlannerRow<'_>,
    ) -> Result<Arc<ClaudePlannerSession>> {
        let track = repo
            .track_get(row.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("track {}", row.track_id)))?;
        let mut instructions =
            crate::operation::planner_harness_start_adapter::planner_instructions(
                repo.as_ref(),
                &self.plugin,
                row.track_id,
                row.card_id,
            )
            .await?;
        instructions.push_str("\n\n");
        instructions.push_str(CLAUDE_FRAGMENT.trim_end());
        let settings = crate::routes::settings::load_settings(repo.as_ref()).await?;
        let proxy = crate::proxy_env::resolved_proxy_env_pairs(
            settings.http_proxy.as_deref(),
            settings.https_proxy.as_deref(),
            |key| std::env::var(key).ok(),
        )
        .into_iter()
        .map(|(upper, lower, value)| (upper.to_string(), lower.to_string(), value))
        .collect();
        let session = ClaudePlannerSession::open(ClaudePlannerSessionParams {
            host: Arc::clone(&self.claude),
            worker_session_id: row.worker_session_id.to_string(),
            card_id: row.card_id.to_string(),
            track_id: row.track_id.to_string(),
            cwd: track.workspace.agent_cwd().into(),
            instructions,
            calm_tools: self.claude.calm_tools.clone(),
            proxy,
            prior_total_tokens: row.prior_total_tokens,
            repo,
            seals,
        })
        .await?;
        Ok(Arc::new(session))
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl ClaudePlannerWiring {
    /// Fixtures only: no config (every Claude turn refuses) and no plugins, for tests that recover
    /// or start Codex harnesses and must still hand recovery its Claude wiring.
    pub fn unconfigured_for_test(repo: Arc<dyn Repo>) -> Self {
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo;
        Self {
            acp: Arc::new(
                crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch()
                    .expect("ACP host"),
            ),
            claude: Arc::new(
                ClaudePlannerHost::unconfigured_scratch().expect("scratch Claude Planner host"),
            ),
            plugin: Arc::new(PluginHost::new_full(
                Arc::new(crate::plugin_host::PluginRegistry::empty()),
                route_repo,
                std::path::PathBuf::new(),
                std::env::temp_dir().join("calm-claude-planner-test-plugins-data"),
                Vec::new(),
                crate::event::EventBus::new(),
                crate::state::WriteContext::new(
                    crate::card_role_cache::CardRoleCache::new(),
                    crate::track_area_cache::TrackAreaCache::new(),
                ),
            )),
        }
    }
}
