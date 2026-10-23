//! Catalog discovery owns no card, submission, MCP credential or prompt.
use super::{
    config::AcpPlannerHost,
    process::{Process, setup_request},
};
use crate::error::{CalmError, Result};
use crate::planner_model::catalog::{
    CatalogModel, DefaultSource, ModelDefaults, ModelSource, ModelsResponse, ReasoningEffortOption,
};
use calm_types::runtime::AgentProvider;
use provider::acp::{configuration::Configuration, protocol};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) const WORKER: &str = "catalog";
const TTL: Duration = Duration::from_secs(30);
type Catalog = (Configuration, i64);
pub(super) type DiscoveryCache = tokio::sync::Mutex<Option<Entry>>;

#[derive(Debug)]
pub(super) struct Entry {
    provider: AgentProvider,
    cwd: Option<PathBuf>,
    started: Instant,
    catalog: Option<Catalog>,
}

impl AcpPlannerHost {
    /// Project the card's current native settings without maintaining another cache.
    pub(crate) fn cached_catalog(&self, card: &str) -> Option<ModelsResponse> {
        self.configuration(card)
            .map(|configuration| project_configuration(Some(configuration)))
    }

    /// Discover independently of a card and project the ACP-owned native cache.
    pub(crate) async fn discover_catalog(
        &self,
        provider: &AgentProvider,
        cwd: Option<&Path>,
    ) -> ModelsResponse {
        project_configuration(self.discover_configuration(provider, cwd).await)
    }

    /// One bounded cache slot, including failures. A workspace change always re-discovers.
    async fn discover_configuration(
        &self,
        provider: &AgentProvider,
        cwd: Option<&Path>,
    ) -> Option<Catalog> {
        // Check registration even before consulting the cache.
        let registration = self.registration(provider).ok()?;
        let key = cwd.map(Path::to_path_buf);
        let mut cache = self.discovery.lock().await;
        if let Some(entry) = cache.as_ref()
            && entry.provider == *provider
            && entry.cwd == key
            && entry.started.elapsed() < TTL
        {
            return entry.catalog.clone();
        }
        let started = Instant::now();
        let result: Result<Configuration> = async {
            let scratch = tempfile::Builder::new()
                .prefix("neige-acp-catalog-")
                .tempdir()?;
            let cwd = cwd.unwrap_or(scratch.path());
            let mut process = Process::spawn(self, registration, WORKER, cwd).await?;
            let discovered = async {
                let result = setup_request(
                    &mut process.connection,
                    "session/new",
                    protocol::encode_new_session(cwd, &[]),
                    &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
                )
                .await?;
                let native: protocol::NewSessionResponse = protocol::decode(result.clone())
                    .map_err(|error| CalmError::Conflict(error.to_string()))?;
                if native.session_id.is_empty() {
                    return Err(CalmError::Conflict(
                        "ACP discovery returned an empty session identity".into(),
                    ));
                }
                // Close the session even if its catalog is malformed. Stop still runs if close fails.
                if process.capabilities.session_capabilities.close.is_some() {
                    setup_request(
                        &mut process.connection,
                        "session/close",
                        protocol::encode_close_session(&native.session_id),
                        &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
                    )
                    .await?;
                }
                Configuration::from_response(&result)
                    .map_err(|error| CalmError::Conflict(error.to_string()))
            }
            .await;
            let stopped = process.stop(self, WORKER).await;
            stopped?;
            discovered
        }
        .await;
        let catalog = match result {
            Ok(configuration) => Some((configuration, crate::model::now_ms())),
            Err(error) => {
                tracing::warn!(%error, "ACP model discovery unavailable");
                None
            }
        };
        *cache = Some(Entry {
            provider: provider.clone(),
            cwd: key,
            started,
            catalog: catalog.clone(),
        });
        catalog
    }
}

fn project_configuration(
    cached: Option<(provider::acp::configuration::Configuration, i64)>,
) -> ModelsResponse {
    let Some((configuration, at)) = cached else {
        return ModelsResponse {
            models: Vec::new(),
            default: ModelDefaults::default(),
            default_source: DefaultSource::Unknown,
            source: ModelSource::Unavailable,
            fetched_at_ms: None,
        };
    };
    let Some(model) = configuration.category("model") else {
        return ModelsResponse {
            models: Vec::new(),
            default: ModelDefaults::default(),
            default_source: DefaultSource::Unknown,
            source: ModelSource::Unavailable,
            fetched_at_ms: Some(at),
        };
    };
    let thought = configuration.category("thought_level");
    let efforts = thought
        .map(|option| {
            option
                .choices()
                .into_iter()
                .map(|choice| ReasoningEffortOption {
                    reasoning_effort: choice.value.clone(),
                    description: Some(choice.name.clone()),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ModelsResponse {
        models: model
            .choices()
            .into_iter()
            .map(|choice| CatalogModel {
                id: choice.value.clone(),
                model: choice.value.clone(),
                resolved_model: None,
                display_name: choice.name.clone(),
                description: String::new(),
                is_default: choice.value == model.current_value,
                supported_reasoning_efforts: if choice.value == model.current_value {
                    efforts.clone()
                } else {
                    Vec::new()
                },
                default_reasoning_effort: if choice.value == model.current_value {
                    thought.map(|option| option.current_value.clone())
                } else {
                    None
                },
            })
            .collect(),
        default: ModelDefaults {
            model: Some(model.current_value.clone()),
            reasoning_effort: thought.map(|option| option.current_value.clone()),
            supported_reasoning_efforts: Some(efforts),
        },
        default_source: DefaultSource::AcpSession,
        source: ModelSource::Live,
        fetched_at_ms: Some(at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn response(native: Value) -> Value {
        serde_json::to_value(project_configuration(Some((
            Configuration::from_response(&native).unwrap(),
            1234,
        ))))
        .unwrap()
    }

    #[test]
    fn unavailable_and_missing_model_keep_distinct_timestamps() {
        assert_eq!(
            serde_json::to_value(project_configuration(None)).unwrap(),
            json!({"models":[],"default":{"model":null,"reasoning_effort":null,
                "supported_reasoning_efforts":null},"default_source":"unknown",
                "source":"unavailable","fetched_at_ms":null})
        );
        assert_eq!(
            response(json!({})),
            json!({"models":[],"default":{"model":null,"reasoning_effort":null,
                "supported_reasoning_efforts":null},"default_source":"unknown",
                "source":"unavailable","fetched_at_ms":1234})
        );
    }

    #[test]
    fn absent_effort_is_live_with_empty_default_efforts() {
        assert_eq!(
            response(
                json!({"configOptions":[{"id":"model-key","category":"model",
                "type":"select","currentValue":"a","options":[{"value":"a","name":"A"}]}]})
            ),
            json!({"models":[{"id":"a","model":"a","resolved_model":null,
                "display_name":"A","description":"","is_default":true,
                "supported_reasoning_efforts":[],"default_reasoning_effort":null}],
                "default":{"model":"a","reasoning_effort":null,"supported_reasoning_efforts":[]},
                "default_source":"acp_session","source":"live","fetched_at_ms":1234})
        );
    }

    #[test]
    fn grouped_choices_follow_changed_selection_and_only_selected_model_has_efforts() {
        let mut native = json!({"configOptions":[
            {"id":"model-key","category":"model","type":"select","currentValue":"a",
                "options":[{"group":"models","options":[{"value":"a","name":"A"},
                    {"value":"b","name":"B"}]}]},
            {"id":"effort-key","category":"thought_level","type":"select","currentValue":"high",
                "options":[{"group":"efforts","options":[{"value":"high","name":"Think hard"}]}]}
        ]});
        assert_eq!(
            response(native.clone()),
            json!({
                "models":[
                    {"id":"a","model":"a","resolved_model":null,"display_name":"A","description":"",
                        "is_default":true,"supported_reasoning_efforts":[{"reasoning_effort":"high","description":"Think hard"}],"default_reasoning_effort":"high"},
                    {"id":"b","model":"b","resolved_model":null,"display_name":"B","description":"",
                        "is_default":false,"supported_reasoning_efforts":[],"default_reasoning_effort":null}],
                "default":{"model":"a","reasoning_effort":"high","supported_reasoning_efforts":[{"reasoning_effort":"high","description":"Think hard"}]},
                "default_source":"acp_session","source":"live","fetched_at_ms":1234
            })
        );
        native["configOptions"][0]["currentValue"] = json!("b");
        assert_eq!(
            response(native),
            json!({
                "models":[
                    {"id":"a","model":"a","resolved_model":null,"display_name":"A","description":"",
                        "is_default":false,"supported_reasoning_efforts":[],"default_reasoning_effort":null},
                    {"id":"b","model":"b","resolved_model":null,"display_name":"B","description":"",
                        "is_default":true,"supported_reasoning_efforts":[{"reasoning_effort":"high","description":"Think hard"}],"default_reasoning_effort":"high"}],
                "default":{"model":"b","reasoning_effort":"high","supported_reasoning_efforts":[{"reasoning_effort":"high","description":"Think hard"}]},
                "default_source":"acp_session","source":"live","fetched_at_ms":1234
            })
        );
    }
}
