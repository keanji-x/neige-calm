//! Route owner completion through the installed harness backend.
use super::*;

impl PlannerHarness {
    pub(crate) async fn provider_transition_gate(&self) -> Result<()> {
        self.inner.backend.provider_transition_gate().await
    }

    pub(crate) async fn complete_provider_execution(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
        purpose: &crate::provider_execution::Purpose,
        outcome: serde_json::Value,
    ) -> Result<()> {
        if matches!(
            purpose,
            crate::provider_execution::Purpose::Compaction { .. }
        ) && self.inner.inputs.canonical().is_some()
        {
            let expected = self.inner.backend.lost_maintenance_target(id, turn).await;
            compact_command::canonical::lost(&self.inner, expected.as_ref(), turn).await?;
        }
        self.inner
            .backend
            .complete_provider_execution(id, turn, outcome)
            .await
    }
}
