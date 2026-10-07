//! `GET /api/models` — the model catalog the planner's model picker reads, plus the
//! default this installation follows. Daemon failures answer 200 with an explicit `source`.
//! A Claude Planner's catalog is the Claude CLI's own model list, fetched and cached by the
//! Claude availability check (#1822).

use crate::extract::{Json, Query};
use std::time::Duration;

use axum::extract::State;
use axum::{Router, routing::get};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::claude_planner::models::{ClaudeCatalog, ClaudeModel};
use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, ErrorBody, Result};
use crate::session_projection_repo::AgentProvider;
use crate::state::{AppState, CodexShellState, RouteState};

/// Budget for the whole request, not each codex read. Passed down into
/// `request_until` rather than wrapped in `tokio::time::timeout`: cancelling from
/// outside leaks the pending-map entry.
pub const CODEX_READ_TIMEOUT: Duration = Duration::from_secs(8);

/// One selectable reasoning effort for a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReasoningEffortOption {
    /// A bare string, never a closed enum: codex's `ReasoningEffort` carries a
    /// `Custom(String)` variant and accepts any non-empty string on the wire.
    pub reasoning_effort: String,
    /// Codex's own words for the effort; `null` for a Claude effort level, which the CLI declares
    /// without a description.
    #[schema(required = true)]
    pub description: Option<String>,
}

/// One entry of the model catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CatalogModel {
    /// Codex's preset identifier (the Claude CLI's `value` for a Claude Planner), presentation
    /// only; never send it back as a model selection.
    pub id: String,
    /// The slug codex is invoked by, or the Claude CLI's `value`, passed verbatim as `--model`.
    pub model: String,
    /// The model this entry runs, where the provider reports one (the Claude CLI's
    /// `resolvedModel`); `null` for Codex, which reports none.
    #[schema(required = true)]
    pub resolved_model: Option<String>,
    pub display_name: String,
    pub description: String,
    /// Codex's catalog-level default marker; not "what am I following now". Always `false` for
    /// a Claude entry: the Claude CLI's default is the `null` selection, described by the
    /// response's `default`.
    pub is_default: bool,
    /// The efforts a selection of this model may carry: Codex's options, or the Claude CLI's
    /// `supportedEffortLevels` (empty when it declares none, and then only `null` is accepted).
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    /// The model's own default effort; `null` exactly when the provider declares none (every
    /// Claude entry: the CLI picks the effort of a `null` selection itself). Always a string for
    /// a Codex entry.
    #[schema(required = true)]
    pub default_reasoning_effort: Option<String>,
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

/// What a card that has selected nothing currently runs; `null` = not configured anywhere readable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ModelDefaults {
    /// For Codex the slug; for a Claude Planner the model the CLI's `default` entry resolves to.
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    /// The efforts a `null` model selection may carry, where the provider describes its default
    /// as an entry of its own (the Claude CLI's `default`: its `supportedEffortLevels`). `null`
    /// for Codex, whose default's efforts are those of the catalog entry `model` names.
    #[schema(required = true)]
    pub supported_reasoning_efforts: Option<Vec<ReasoningEffortOption>>,
}

/// Where [`ModelsResponse::default`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// The daemon's layer-merged effective config for the requested card's workspace.
    ConfigRead,
    /// The shared CODEX_HOME `config.toml`, read directly when no daemon connection exists;
    /// weaker than `config_read` because a managed-config layer can override it.
    ConfigToml,
    /// The Claude CLI's own `default` entry in its model list (#1822): `default.model` is the model
    /// it resolves to. Only for a Claude Planner.
    ClaudeCli,
    /// The managed agent's declared current session settings, read during ACP setup.
    AcpSession,
    /// No default could be established: the read failed, no `card_id` was supplied, or a Claude
    /// Planner's CLI is not ready.
    Unknown,
}

/// Whether `models` reflects a live catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Codex answered, or, for a Claude Planner, the Claude CLI's own model list as the ready
    /// availability check cached it. A Codex `models` may still be empty, which is different from
    /// "could not ask".
    Live,
    /// Codex could not be asked, or a Claude Planner is not ready (`GET /api/agent-providers`
    /// says why). `models` is empty for lack of an answer; there is never a hardcoded list.
    Unavailable,
}

/// Response body for `GET /api/models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ModelsResponse {
    pub models: Vec<CatalogModel>,
    pub default: ModelDefaults,
    pub default_source: DefaultSource,
    pub source: ModelSource,
    /// Wall-clock ms at which the catalog was fetched (for a Claude Planner, when the cached list
    /// was), or `null` when it was not fetched.
    pub fetched_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ModelsQuery {
    /// Resolve the default against this card's workspace; without one the read has no
    /// `cwd` and `default_source` is `unknown`.
    pub card_id: Option<String>,
    /// The Planner provider a card is about to be created with. `claude` (like a `card_id`
    /// naming a Claude Planner card) answers the Claude CLI's cached model list with
    /// `source: "live"` without asking Codex, or `source: "unavailable"` with an empty catalog
    /// while Claude is not ready.
    pub provider: Option<AgentProvider>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/models", get(list_models))
}

#[utoipa::path(
    get,
    path = "/api/models",
    tag = "models",
    params(ModelsQuery),
    responses(
        (status = 200, description = "Model catalog and the default this installation follows. Codex: `source: \"live\"`, or `source: \"unavailable\"` with an empty catalog if codex cannot be reached (never an error or a hardcoded list). A Claude Planner: `source: \"live\"` with the Claude CLI's cached model list (its `default` entry as `default`, `default_source: \"claude_cli\"`), or `unavailable` with an empty catalog while Claude is not ready", body = ModelsResponse),
        (status = 404, description = "`card_id` names a card that does not exist", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_models(
    State(s): State<RouteState>,
    State(codex): State<CodexShellState>,
    Query(q): Query<ModelsQuery>,
) -> Result<Json<ModelsResponse>> {
    // Resolved before codex is asked so a bad `card_id` is rejected regardless of daemon
    // state. A blank `card_id=` means "no card supplied".
    let card = match q
        .card_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(card_id) => Some(resolve_card_workspace(&s, card_id).await?),
        None => None,
    };
    if q.provider == Some(AgentProvider::OpenCode)
        || card
            .as_ref()
            .is_some_and(|card| card.provider == AgentProvider::OpenCode)
    {
        return Ok(Json(acp_catalog(
            q.card_id
                .as_deref()
                .and_then(|id| s.acp_planner.configuration(id)),
        )));
    }
    // #1822: a Claude Planner's catalog is the CLI's own list, cached by its availability
    // check, and Codex is not asked. A Claude that is not ready has no list to offer.
    if q.provider == Some(AgentProvider::Claude)
        || card
            .as_ref()
            .is_some_and(|card| card.provider == AgentProvider::Claude)
    {
        let checked = s
            .provider_availability
            .claude(crate::agent_providers::Freshness::Cached, &s.claude_planner)
            .await;
        return Ok(Json(match checked.catalog() {
            Ok(catalog) => claude_catalog(&catalog),
            Err(_) => ModelsResponse {
                models: Vec::new(),
                default: ModelDefaults::default(),
                default_source: DefaultSource::Unknown,
                source: ModelSource::Unavailable,
                fetched_at_ms: None,
            },
        }));
    }
    let cwd = card.map(|card| card.workspace);
    let daemon = &codex.shared_codex_appserver;
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
        match cwd.as_deref() {
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

    Ok(Json(ModelsResponse {
        models,
        default,
        default_source,
        source,
        fetched_at_ms,
    }))
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

fn acp_catalog(
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

/// A complete `config/read` whose `model` is unset is `config_read` + `null`, not `unknown`.
fn defaults_from_config_read(config: CodexConfig) -> ModelDefaults {
    ModelDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
        supported_reasoning_efforts: None,
    }
}

/// A card's workspace and whether it is a Claude Planner card.
struct ResolvedCard {
    workspace: String,
    provider: AgentProvider,
}

/// The workspace path a card's codex thread runs in — the same value
/// `planner-harness-start` puts on its payload. Any card resolves, not only a planner card.
async fn resolve_card_workspace(s: &RouteState, card_id: &str) -> Result<ResolvedCard> {
    let card = s
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let provider = s
        .write
        .verify_role(&card.id)
        .and_then(|role| crate::harness::profile::PlannerBinding::from_card(&card, role))
        .map_or(AgentProvider::Codex, |binding| binding.provider);
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("track {} for card {card_id}", card.track_id))
        })?;
    Ok(ResolvedCard {
        workspace: track.workspace.agent_cwd().to_string(),
        provider,
    })
}
