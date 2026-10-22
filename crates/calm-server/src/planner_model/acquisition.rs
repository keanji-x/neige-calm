//! Catalog acquisition and advice for the five selection entry contracts.
use std::sync::Arc;

use super::catalog::*;
use super::selection_advice::{CatalogAdvice, CatalogSource, catalog_advice};
use crate::agent_providers::ProviderAvailabilityCache;
use crate::claude_planner::models::ClaudeCatalog;
use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, Result};
use crate::harness::backend::factory::{CatalogReadiness, PlannerFactory};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::SharedCodexAppServer;
use calm_types::runtime::AgentProtocol;

/// Differences belong here, rather than in provider branches in callers.
enum EntryContext<'a> {
    CatalogRead {
        card: Option<&'a str>,
        cwd: Option<&'a str>,
    },
    ModelWrite {
        card: &'a str,
    },
    HistoricalMint(&'a CatalogReadiness),
    CanonicalMint,
    DelegatedChild {
        cwd: &'a str,
    },
}

enum Acquired {
    Codex,
    Claude(Arc<ClaudeCatalog>),
    Discovered(ModelsResponse),
    Unjudged,
}

pub(crate) struct CatalogAccess<'a> {
    availability: &'a ProviderAvailabilityCache,
    claude: &'a Arc<crate::claude_planner::config::ClaudePlannerHost>,
    acp: &'a Arc<crate::acp_planner::config::AcpPlannerHost>,
    codex: &'a Arc<SharedCodexAppServer>,
}

impl<'a> CatalogAccess<'a> {
    pub(crate) fn new(
        availability: &'a ProviderAvailabilityCache,
        claude: &'a Arc<crate::claude_planner::config::ClaudePlannerHost>,
        acp: &'a Arc<crate::acp_planner::config::AcpPlannerHost>,
        codex: &'a Arc<SharedCodexAppServer>,
    ) -> Self {
        Self {
            availability,
            claude,
            acp,
            codex,
        }
    }

    async fn acquire(&self, provider: &AgentProvider, entry: EntryContext<'_>) -> Result<Acquired> {
        match crate::harness::PlannerBackend::metadata(provider).catalog {
            AgentProtocol::Codex => match entry {
                EntryContext::CatalogRead { card: _, cwd } => {
                    Ok(Acquired::Discovered(codex_catalog(self.codex, cwd).await))
                }
                EntryContext::HistoricalMint(readiness) => {
                    readiness.historical_catalog()?;
                    Ok(Acquired::Codex)
                }
                EntryContext::ModelWrite { card: _ }
                | EntryContext::CanonicalMint
                | EntryContext::DelegatedChild { cwd: _ } => Ok(Acquired::Codex),
            },
            AgentProtocol::Claude => {
                let sampled;
                let readiness = match &entry {
                    EntryContext::HistoricalMint(readiness) => *readiness,
                    EntryContext::CatalogRead { card: _, cwd: _ }
                    | EntryContext::ModelWrite { card: _ }
                    | EntryContext::CanonicalMint
                    | EntryContext::DelegatedChild { cwd: _ } => {
                        sampled =
                            PlannerFactory::new(Arc::downgrade(self.codex), self.claude, self.acp)
                                .catalog_readiness(provider, self.availability)
                                .await;
                        &sampled
                    }
                };
                // Factory selected the readiness mechanism; this owner converts its
                // factual result according to the entry's refusal/advisory contract.
                let catalog = match readiness {
                    CatalogReadiness::Claude(catalog) => catalog,
                    CatalogReadiness::Native | CatalogReadiness::Acp(_) => {
                        return Err(CalmError::Internal(
                            "Claude catalog readiness missing".into(),
                        ));
                    }
                };
                match (catalog, entry) {
                    (
                        Ok(catalog),
                        EntryContext::CatalogRead { card: _, cwd: _ }
                        | EntryContext::ModelWrite { card: _ }
                        | EntryContext::HistoricalMint(_)
                        | EntryContext::CanonicalMint
                        | EntryContext::DelegatedChild { cwd: _ },
                    ) => Ok(Acquired::Claude(Arc::clone(catalog))),
                    (Err(_), EntryContext::CatalogRead { card: _, cwd: _ }) => {
                        Ok(Acquired::Discovered(unavailable()))
                    }
                    (Err(_), EntryContext::CanonicalMint) => Ok(Acquired::Unjudged),
                    (Err(reason), EntryContext::ModelWrite { card }) => Err(CalmError::BadRequest(
                        format!("card {card}: `planner_provider` {reason}"),
                    )),
                    (Err(reason), EntryContext::HistoricalMint(_)) => Err(CalmError::BadRequest(
                        format!("track create: `planner_provider` {reason}"),
                    )),
                    (Err(reason), EntryContext::DelegatedChild { cwd: _ }) => Err(
                        CalmError::BadRequest(format!("child_track.provider: {reason}")),
                    ),
                }
            }
            AgentProtocol::Acp => match entry {
                EntryContext::CatalogRead { card, cwd } => {
                    let catalog = match card.and_then(|id| self.acp.cached_catalog(id)) {
                        Some(catalog) => catalog,
                        None => {
                            self.acp
                                .discover_catalog(provider, cwd.map(std::path::Path::new))
                                .await
                        }
                    };
                    Ok(Acquired::Discovered(catalog))
                }
                EntryContext::ModelWrite { card: _ } => {
                    self.acp.configured(provider)?;
                    Ok(Acquired::Unjudged)
                }
                EntryContext::HistoricalMint(readiness) => {
                    readiness.historical_catalog()?;
                    Ok(Acquired::Unjudged)
                }
                EntryContext::CanonicalMint => Ok(Acquired::Discovered(
                    self.acp.discover_catalog(provider, None).await,
                )),
                EntryContext::DelegatedChild { cwd } => {
                    let ready =
                        PlannerFactory::new(Arc::downgrade(self.codex), self.claude, self.acp)
                            .catalog_readiness(provider, self.availability)
                            .await;
                    match ready {
                        CatalogReadiness::Acp(ready) => ready.map_err(|reason| {
                            CalmError::BadRequest(format!("child_track.provider: {reason}"))
                        })?,
                        CatalogReadiness::Native | CatalogReadiness::Claude(_) => {
                            return Err(CalmError::Internal(
                                "ACP catalog readiness missing".into(),
                            ));
                        }
                    }
                    Ok(Acquired::Discovered(
                        self.acp
                            .discover_catalog(provider, Some(std::path::Path::new(cwd)))
                            .await,
                    ))
                }
            },
        }
    }

    pub(crate) async fn read(
        &self,
        provider: &AgentProvider,
        card: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<ModelsResponse> {
        // Read acquisition never refuses readiness; route authorization precedes this call.
        match self
            .acquire(provider, EntryContext::CatalogRead { card, cwd })
            .await?
        {
            Acquired::Discovered(catalog) => Ok(catalog),
            Acquired::Claude(catalog) => {
                Ok(crate::claude_planner::catalog::claude_catalog(&catalog))
            }
            Acquired::Codex | Acquired::Unjudged => Err(CalmError::Internal(
                "catalog read did not acquire a projection".into(),
            )),
        }
    }

    async fn advice(
        &self,
        provider: &AgentProvider,
        entry: EntryContext<'_>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<CatalogAdvice> {
        Ok(match self.acquire(provider, entry).await? {
            Acquired::Codex => {
                catalog_advice(CatalogSource::Codex(self.codex), model, effort).await
            }
            Acquired::Claude(catalog) => {
                catalog_advice(CatalogSource::Claude(&catalog), model, effort).await
            }
            Acquired::Discovered(catalog) => {
                catalog_advice(CatalogSource::Discovered(&catalog), model, effort).await
            }
            Acquired::Unjudged => CatalogAdvice::default(),
        })
    }

    pub(crate) async fn model_write(
        &self,
        provider: &AgentProvider,
        card: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<CatalogAdvice> {
        self.advice(provider, EntryContext::ModelWrite { card }, model, effort)
            .await
    }

    pub(crate) async fn canonical_mint(
        &self,
        provider: &AgentProvider,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<CatalogAdvice> {
        self.advice(provider, EntryContext::CanonicalMint, model, effort)
            .await
    }

    pub(crate) async fn historical_mint(
        &self,
        provider: &AgentProvider,
        readiness: &CatalogReadiness,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<CatalogAdvice> {
        self.advice(
            provider,
            EntryContext::HistoricalMint(readiness),
            model,
            effort,
        )
        .await
    }

    pub(crate) async fn child(
        &self,
        provider: &AgentProvider,
        cwd: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<CatalogAdvice> {
        self.advice(
            provider,
            EntryContext::DelegatedChild { cwd },
            model,
            effort,
        )
        .await
    }
}

fn unavailable() -> ModelsResponse {
    ModelsResponse {
        models: Vec::new(),
        default: ModelDefaults::default(),
        default_source: DefaultSource::Unknown,
        source: ModelSource::Unavailable,
        fetched_at_ms: None,
    }
}
impl From<CodexModel> for CatalogModel {
    fn from(m: CodexModel) -> Self {
        Self {
            id: m.id,
            model: m.model,
            resolved_model: None,
            display_name: m.display_name,
            description: m.description,
            is_default: m.is_default,
            supported_reasoning_efforts: m
                .supported_reasoning_efforts
                .into_iter()
                .map(|o| ReasoningEffortOption {
                    reasoning_effort: o.reasoning_effort,
                    description: Some(o.description),
                })
                .collect(),
            default_reasoning_effort: Some(m.default_reasoning_effort),
        }
    }
}

async fn codex_catalog(daemon: &SharedCodexAppServer, cwd: Option<&str>) -> ModelsResponse {
    let deadline = tokio::time::Instant::now() + CODEX_READ_TIMEOUT;

    let listed = match daemon.model_list(deadline).await {
        Ok(models) => Some(models),
        Err(e) => {
            tracing::warn!(error = %e, "GET /api/models: model/list unavailable");
            None
        }
    };

    let (models, source, fetched_at_ms) = match listed {
        Some(models) => (
            models.into_iter().map(CatalogModel::from).collect(),
            ModelSource::Live,
            Some(crate::model::now_ms()),
        ),
        None => (Vec::new(), ModelSource::Unavailable, None),
    };

    // Chosen by whether a CONNECTION exists, not by whether `model/list` succeeded: a
    // live daemon whose catalog failed must not fall back to `config.toml`.
    let connected = daemon.has_connection().await;
    let (default, default_source) = if connected {
        match cwd {
            Some(cwd) => match daemon.config_read(Some(cwd), deadline).await {
                Ok(config) => (defaults_from_config_read(config), DefaultSource::ConfigRead),
                Err(e) => {
                    tracing::warn!(error = %e, "GET /api/models: config/read failed");
                    (ModelDefaults::default(), DefaultSource::Unknown)
                }
            },
            None => (ModelDefaults::default(), DefaultSource::Unknown),
        }
    } else {
        match daemon.shared_home().read_default_model_settings() {
            Ok(toml) => (
                ModelDefaults {
                    model: toml.model,
                    reasoning_effort: toml.reasoning_effort,
                    supported_reasoning_efforts: None,
                },
                DefaultSource::ConfigToml,
            ),
            Err(e) => {
                tracing::warn!(error = %e, "GET /api/models: shared config.toml unreadable");
                (ModelDefaults::default(), DefaultSource::Unknown)
            }
        }
    };

    ModelsResponse {
        models,
        default,
        default_source,
        source,
        fetched_at_ms,
    }
}

/// A complete `config/read` whose `model` is unset is `config_read` + `null`, not `unknown`.
fn defaults_from_config_read(config: CodexConfig) -> ModelDefaults {
    ModelDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
        supported_reasoning_efforts: None,
    }
}
