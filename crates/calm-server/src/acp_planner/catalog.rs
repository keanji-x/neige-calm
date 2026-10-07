//! Catalog discovery owns no card, submission, MCP credential or prompt.
use super::{
    config::AcpPlannerHost,
    process::{LaunchContext, Process, setup_request},
};
use crate::error::{CalmError, Result};
use calm_types::runtime::AgentProvider;
use provider::acp::{configuration::Configuration, protocol};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) const WORKER: &str = "catalog";
const TTL: Duration = Duration::from_secs(30);
type Catalog = (Configuration, i64);
pub(super) type DiscoveryCache = tokio::sync::Mutex<Option<Entry>>;

#[derive(Debug)]
pub(super) struct Entry {
    cwd: Option<PathBuf>,
    started: Instant,
    catalog: Option<Catalog>,
}

impl AcpPlannerHost {
    /// One bounded cache slot, including failures. A workspace change always re-discovers.
    pub async fn discover_configuration(
        &self,
        provider: &AgentProvider,
        cwd: Option<&Path>,
    ) -> Option<Catalog> {
        // Check registration even before consulting the cache.
        let config = self.configured(provider).ok()?;
        let key = cwd.map(Path::to_path_buf);
        let mut cache = self.discovery.lock().await;
        if let Some(entry) = cache.as_ref()
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
            let mut process =
                Process::spawn(self, config, WORKER, cwd, LaunchContext::Readiness).await?;
            let discovered = async {
                let result = setup_request(
                    &mut process,
                    "session/new",
                    json!({"cwd":cwd,"mcpServers":[]}),
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
                        &mut process,
                        "session/close",
                        json!({"sessionId":native.session_id}),
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
            cwd: key,
            started,
            catalog: catalog.clone(),
        });
        catalog
    }
}
