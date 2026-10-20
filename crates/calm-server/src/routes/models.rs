//! `GET /api/models` — the model catalog the planner's model picker reads, plus the
//! default this installation follows. Daemon failures answer 200 with an explicit `source`.
//! A Claude Planner's catalog is the Claude CLI's own model list, fetched and cached by the
//! Claude availability check (#1822).

use crate::extract::{Json, Query};
use crate::harness::profile::HarnessProfileExt;
use crate::planner_model::catalog::{
    CatalogModel, DefaultSource, ModelDefaults, ModelSource, ModelsResponse, ReasoningEffortOption,
};

use axum::extract::State;
use axum::{Router, routing::get};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, ErrorBody, Result};
use crate::session_projection_repo::AgentProvider;
use crate::state::{AppState, CodexShellState, RouteState};
use calm_types::runtime::AgentProtocol;

/// Budget for the whole request, not each codex read. Passed down into
/// `request_until` rather than wrapped in `tokio::time::timeout`: cancelling from
/// outside leaks the pending-map entry.
pub use crate::planner_model::catalog::CODEX_READ_TIMEOUT;

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
    if let (Some(card), Some(requested)) = (card.as_ref(), q.provider.as_ref())
        && &card.provider != requested
    {
        return Err(CalmError::BadRequest(
            "The catalog provider does not match this conversation".into(),
        ));
    }
    // A card's immutable binding owns its catalog; a query cannot override it.
    let provider = card
        .as_ref()
        .map(|card| card.provider.clone())
        .or(q.provider.clone())
        .unwrap_or(AgentProvider::Codex);
    let catalog = crate::harness::backend::PlannerBackend::metadata(&provider).catalog;
    match catalog {
        AgentProtocol::Acp => {
            let cached = q
                .card_id
                .as_deref()
                .and_then(|id| s.acp_planner.cached_catalog(id));
            let response = match cached {
                Some(response) => response,
                None => {
                    s.acp_planner
                        .discover_catalog(
                            &provider,
                            card.as_ref()
                                .map(|card| std::path::Path::new(&card.workspace)),
                        )
                        .await
                }
            };
            return Ok(Json(response));
        }
        // #1822: a Claude Planner's catalog is the CLI's own list, cached by its availability
        // check, and Codex is not asked. A Claude that is not ready has no list to offer.
        AgentProtocol::Claude => {
            return Ok(Json(
                crate::claude_planner::catalog::cached_catalog(
                    &s.provider_availability,
                    &s.claude_planner,
                )
                .await,
            ));
        }
        AgentProtocol::Codex => {}
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
    let role = s.write.verify_role(&card.id);
    let provider = match role {
        Some(role)
            if crate::harness::profile::HarnessProfile::from_shape(
                &card.kind,
                role,
                &card.payload,
            )
            .is_some()
                || role == crate::model::CardRole::Assistant =>
        {
            crate::harness::profile::checked_card_binding(s.repo.as_ref(), &card, role)
                .await?
                .provider
        }
        _ => AgentProvider::Codex,
    };
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
