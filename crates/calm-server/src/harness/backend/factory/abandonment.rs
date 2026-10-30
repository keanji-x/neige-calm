//! Provider-owned qualifications for explicit carrier retirement.
use super::*;
use crate::db::sqlite::conversation::{AbandonmentMode, CurrentInputCarrier};
use calm_types::conversation::AbandonConversationRequest;

pub(crate) struct AbandonmentTarget {
    pub(crate) mode: AbandonmentMode,
    pub(crate) request: AbandonConversationRequest,
}
impl PlannerFactory<'_> {
    pub(crate) async fn abandonment_target_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<Option<AbandonmentTarget>> {
        let (binding, _) = crate::harness::profile::input_carrier_contract_tx(tx, carrier).await?;
        let (mode, request) = match binding.provider.protocol() {
            AgentProtocol::Acp => (
                AbandonmentMode::UnknownInput,
                crate::acp_planner::lifecycle::abandonment_request_tx(tx, carrier).await?,
            ),
            AgentProtocol::Claude => (
                AbandonmentMode::UnconfirmedSession,
                crate::claude_planner::recovery::abandonment_request_tx(tx, carrier).await?,
            ),
            AgentProtocol::Codex => return Ok(None),
        };
        Ok(request.map(|request| AbandonmentTarget { mode, request }))
    }
    pub(crate) async fn quiesce_for_abandonment(
        &self,
        provider: &AgentProvider,
        worker: &str,
        track: &str,
    ) -> Result<crate::provider_execution::quiescence::Quiescence> {
        match provider.protocol() {
            AgentProtocol::Acp => {
                crate::acp_planner::lifecycle::quiesce_for_abandonment(self.acp, worker, track)
                    .await
            }
            AgentProtocol::Claude => {
                crate::claude_planner::recovery::quiesce_for_abandonment(self.claude, worker, track)
                    .await
            }
            AgentProtocol::Codex => Err(CalmError::Conflict(
                "This provider does not support session abandonment".into(),
            )),
        }
    }
}
