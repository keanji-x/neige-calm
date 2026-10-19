//! Claude's model API projection, read from the authoritative availability cache.
use super::{config::ClaudePlannerHost, models::ClaudeCatalog};
use crate::agent_providers::{Freshness, ProviderAvailabilityCache};
use crate::planner_model::catalog::{
    CatalogModel, DefaultSource, ModelDefaults, ModelSource, ModelsResponse, ReasoningEffortOption,
};
use provider::claude::discovery::ClaudeModel;

/// Read cached readiness with its existing TTL policy; keep no separate projection cache.
pub(crate) async fn cached_catalog(
    availability: &ProviderAvailabilityCache,
    host: &ClaudePlannerHost,
) -> ModelsResponse {
    let checked = availability.claude(Freshness::Cached, host).await;
    match checked.catalog() {
        Ok(catalog) => claude_catalog(&catalog),
        Err(_) => ModelsResponse {
            models: Vec::new(),
            default: ModelDefaults::default(),
            default_source: DefaultSource::Unknown,
            source: ModelSource::Unavailable,
            fetched_at_ms: None,
        },
    }
}

impl From<&ClaudeModel> for CatalogModel {
    fn from(m: &ClaudeModel) -> Self {
        Self {
            id: m.value.clone(),
            model: m.value.clone(),
            resolved_model: Some(m.resolved_model.clone()),
            display_name: m.display_name.clone(),
            description: m.description.clone(),
            is_default: false,
            supported_reasoning_efforts: claude_efforts(m),
            default_reasoning_effort: None,
        }
    }
}

fn claude_efforts(m: &ClaudeModel) -> Vec<ReasoningEffortOption> {
    m.effort_levels
        .iter()
        .map(|level| ReasoningEffortOption {
            reasoning_effort: level.clone(),
            description: None,
        })
        .collect()
}

/// The answer for a Claude Planner: every listed entry, and the CLI's `default` entry as the
/// default (what it resolves to, and its effort levels).
fn claude_catalog(catalog: &ClaudeCatalog) -> ModelsResponse {
    ModelsResponse {
        models: catalog.models.iter().map(CatalogModel::from).collect(),
        default: ModelDefaults {
            model: Some(catalog.default.resolved_model.clone()),
            reasoning_effort: None,
            supported_reasoning_efforts: Some(claude_efforts(&catalog.default)),
        },
        default_source: DefaultSource::ClaudeCli,
        source: ModelSource::Live,
        fetched_at_ms: Some(catalog.fetched_at_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_default_effort_list_stays_an_array_and_unknown_efforts_keep_their_order() {
        let catalog = ClaudeCatalog {
            default: ClaudeModel {
                value: "default".into(),
                resolved_model: "resolved-default".into(),
                display_name: "Default".into(),
                description: String::new(),
                effort_levels: Vec::new(),
            },
            models: vec![ClaudeModel {
                value: "alias".into(),
                resolved_model: "resolved-alias".into(),
                display_name: "Alias".into(),
                description: "Description".into(),
                effort_levels: vec!["future-effort".into(), "low".into()],
            }],
            fetched_at_ms: 1234,
        };
        assert_eq!(
            serde_json::to_value(claude_catalog(&catalog)).unwrap(),
            json!({
                "models": [{"id":"alias", "model":"alias", "resolved_model":"resolved-alias",
                    "display_name":"Alias", "description":"Description", "is_default":false,
                    "supported_reasoning_efforts":[
                        {"reasoning_effort":"future-effort", "description":null},
                        {"reasoning_effort":"low", "description":null}],
                    "default_reasoning_effort":null}],
                "default":{"model":"resolved-default", "reasoning_effort":null,
                    "supported_reasoning_efforts":[]},
                "default_source":"claude_cli", "source":"live", "fetched_at_ms":1234
            })
        );
    }
}
