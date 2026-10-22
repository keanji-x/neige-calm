//! Selection-time readiness snapshots reuse the authoritative availability cache.
use super::*;
use crate::agent_providers::{Freshness, ProviderAvailabilityCache};
use crate::claude_planner::models::ClaudeCatalog;

/// Sampling and enforcing are separate: historical receipts win over readiness.
pub(crate) enum CatalogReadiness {
    Native,
    Claude(std::result::Result<Arc<ClaudeCatalog>, String>),
    Acp(std::result::Result<(), String>),
}

impl CatalogReadiness {
    pub(crate) fn historical_catalog(&self) -> Result<Option<&ClaudeCatalog>> {
        match self {
            Self::Native => Ok(None),
            Self::Claude(Ok(catalog)) => Ok(Some(catalog)),
            Self::Claude(Err(reason)) => Err(CalmError::BadRequest(format!(
                "track create: `planner_provider` {reason}"
            ))),
            Self::Acp(ready) => ready
                .as_ref()
                .map(|()| None)
                .map_err(|reason| CalmError::BadRequest(format!("track create: {reason}"))),
        }
    }

    pub(crate) fn require_added_track(&self) -> std::result::Result<(), String> {
        match self {
            Self::Native | Self::Claude(Ok(_)) | Self::Acp(Ok(())) => Ok(()),
            Self::Claude(Err(reason)) => Err(format!("the creator's Planner provider {reason}")),
            Self::Acp(Err(reason)) => Err(reason.clone()),
        }
    }
}

impl PlannerFactory<'_> {
    /// Codex creation is advisory, Claude needs its cached CLI catalog, and ACP
    /// needs its registration's readiness. No additional cache or eager native start.
    pub(crate) async fn catalog_readiness(
        &self,
        provider: &AgentProvider,
        availability: &ProviderAvailabilityCache,
    ) -> CatalogReadiness {
        match provider.protocol() {
            AgentProtocol::Codex => CatalogReadiness::Native,
            AgentProtocol::Claude => CatalogReadiness::Claude(
                availability
                    .claude(Freshness::Cached, self.claude)
                    .await
                    .catalog(),
            ),
            AgentProtocol::Acp => CatalogReadiness::Acp(
                availability
                    .acp(provider, Freshness::Cached, self.acp)
                    .await
                    .require_ready(),
            ),
        }
    }
}
