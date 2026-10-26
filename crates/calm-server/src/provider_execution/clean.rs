//! Boot-owned pre-C4 evidence consumed by closed-track clean, including recovery.
use super::*;

#[derive(Clone)]
pub(crate) struct PlannerCleanProof {
    claude: Arc<crate::claude_planner::config::ClaudePlannerHost>,
    acp: Arc<crate::acp_planner::config::AcpPlannerHost>,
}
impl PlannerCleanProof {
    pub(crate) fn new(
        claude: Arc<crate::claude_planner::config::ClaudePlannerHost>,
        acp: Arc<crate::acp_planner::config::AcpPlannerHost>,
    ) -> Self {
        Self { claude, acp }
    }

    /// Uninitialized proof keeps the proxy. A failed sweep keeps its boot-time
    /// sessions unproven; later sessions have no pre-C4 process by construction.
    pub(crate) fn unproven(&self) -> Option<Vec<String>> {
        let claude = self.claude.pre_c4.get()?;
        let acp = self.acp.pre_c4.get()?;
        Some(claude.unproven.union(&acp.unproven).cloned().collect())
    }
}
