//! Creation-time provider admission, shared catalog policy and explicit override merge.
use super::CardModelSelection;
use crate::agent_providers::ProviderAvailabilityCache;
use crate::error::{CalmError, Result};
use crate::session_projection_repo::AgentProvider;
use calm_types::report_blocks::tasks::ChildTrackConfig;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct ChildSelectionContext {
    pub availability: Arc<ProviderAvailabilityCache>,
    pub claude: Arc<crate::claude_planner::config::ClaudePlannerHost>,
    pub acp: Arc<crate::acp_planner::config::AcpPlannerHost>,
    pub codex: Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
}

/// Provider change resets both selection halves; omitted fields otherwise preserve history.
pub(crate) fn merge(
    selection: &mut CardModelSelection,
    provider: &mut AgentProvider,
    declared: &ChildTrackConfig,
) -> Result<()> {
    if let Some(name) = &declared.provider {
        let chosen: AgentProvider = serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|e| CalmError::BadRequest(format!("child_track.provider: {e}")))?;
        if chosen != *provider {
            *selection = CardModelSelection::default();
        }
        *provider = chosen;
    }
    if let Some(model) = &declared.model {
        selection.model = Some(model.clone());
        selection.model_ever_set = true;
    }
    if let Some(effort) = &declared.reasoning_effort {
        selection.reasoning_effort = Some(effort.clone());
        selection.reasoning_effort_ever_set = true;
    }
    Ok(())
}

impl ChildSelectionContext {
    pub async fn validate(
        &self,
        provider: &AgentProvider,
        selection: &CardModelSelection,
        declared: &ChildTrackConfig,
        cwd: &str,
    ) -> Result<()> {
        // Readiness uses the same cache and refusal contracts as independent creation.
        let model = selection.model.as_deref();
        let effort = selection.reasoning_effort.as_deref();
        let advice = super::acquisition::CatalogAccess::new(
            &self.availability,
            &self.claude,
            &self.acp,
            &self.codex,
        )
        .child(provider, cwd, model, effort)
        .await?;
        if declared.model.is_some() {
            advice.require_child_selection(model, effort)
        } else if declared.reasoning_effort.is_some() {
            advice.require_create_selection(model, effort)
        } else {
            Ok(())
        }
    }
}
