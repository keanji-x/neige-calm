//! Provider-owned qualifications for explicit carrier retirement.
use super::*;
use crate::db::sqlite::conversation::{AbandonmentMode, CurrentInputCarrier};
use calm_types::conversation::AbandonConversationRequest;

pub(crate) enum ProviderQuiescence {
    Acp(crate::acp_planner::lifecycle::AbandonmentProof),
    Claude(crate::provider_execution::quiescence::Quiescence),
}

impl ProviderQuiescence {
    pub(crate) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        match self {
            Self::Acp(proof) => proof.recheck_tx(tx).await,
            Self::Claude(proof) => proof.recheck_tx(tx).await,
        }
    }
}

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
        submission: &str,
    ) -> Result<ProviderQuiescence> {
        match provider.protocol() {
            AgentProtocol::Acp => crate::acp_planner::lifecycle::quiesce_for_abandonment(
                self.acp, provider, worker, track, submission,
            )
            .await
            .map(ProviderQuiescence::Acp),
            AgentProtocol::Claude => {
                crate::claude_planner::recovery::quiesce_for_abandonment(self.claude, worker, track)
                    .await
                    .map(ProviderQuiescence::Claude)
            }
            AgentProtocol::Codex => Err(CalmError::Conflict(
                "This provider does not support session abandonment".into(),
            )),
        }
    }
}
