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
    pub worker_session_id: &'a str,
    pub card_id: &'a str,
    pub track_id: &'a str,
    pub prior_total_tokens: i64,
}
