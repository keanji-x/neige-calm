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
    fn unproven(&self) -> Option<Vec<String>> {
        let claude = self.claude.pre_c4.get()?;
        let acp = self.acp.pre_c4.get()?;
        Some(claude.unproven.union(&acp.unproven).cloned().collect())
    }

    pub(crate) async fn evidence(
        &self,
        pool: &sqlx::SqlitePool,
    ) -> Result<calm_truth::db::sqlite::PlannerCleanEvidence> {
        use calm_truth::db::sqlite::PlannerCleanEvidence;
        let Some(unproven) = self.unproven() else {
            return Ok(PlannerCleanEvidence::Unknown);
        };
        // Audited capability: only Claude has no track-cwd process outside
        // ProviderExecution occupancy. ACP catalog discovery does; undeclared
        // providers retain their proxy. Expanding this requires a spawn-path audit.
        let provider = calm_types::runtime::AgentProvider::Claude;
        let history =
            executions::settlement_history(pool, executions::PLANNER_ALIAS_PREFIX).await?;
        let descendant_complete = history
            .into_iter()
            .filter_map(|(id, json)| {
                let settlement: Settlement = serde_json::from_str(json.as_deref()?).ok()?;
                settlement.descendants_complete().then_some(id)
            })
            .collect();
        Ok(PlannerCleanEvidence::Swept {
            provider,
            unproven,
            descendant_complete,
        })
    }
}
