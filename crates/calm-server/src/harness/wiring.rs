//! Server-owned construction facts shared by managed Planner backends.
use crate::acp_planner::config::AcpPlannerHost;
use crate::claude_planner::config::ClaudePlannerHost;
use crate::plugin_host::PluginHost;
use std::sync::Arc;

#[derive(Clone)]
pub struct PlannerWiring {
    pub claude: Arc<ClaudePlannerHost>,
    pub acp: Arc<AcpPlannerHost>,
    pub plugin: Arc<PluginHost>,
}

pub struct PlannerRow<'a> {
    pub binding: crate::harness::profile::PlannerBinding,
    pub worker_session_id: &'a str,
    pub card_id: &'a str,
    pub track_id: &'a str,
    pub prior_total_tokens: i64,
}

#[cfg(any(test, feature = "fixtures"))]
impl PlannerWiring {
    /// Fixtures only: unconfigured local providers and no plugins, for tests that recover
    /// or start managed harnesses.
    pub fn unconfigured_for_test(repo: Arc<dyn crate::db::Repo>) -> Self {
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo;
        Self {
            acp: Arc::new(
                crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch()
                    .expect("ACP host"),
            ),
            claude: Arc::new(
                crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch()
                    .expect("scratch Claude Planner host"),
            ),
            plugin: Arc::new(crate::plugin_host::PluginHost::new_full(
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
