//! `GET /api/models` — the model catalog the planner's model picker reads, plus the
//! default this installation follows. Daemon failures answer 200 with an explicit `source`.
//! A Claude Planner's catalog is the fixed alias list of `claude_planner::models` (#1810).

use std::time::Duration;

use axum::extract::{Query, State};
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

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
    pub description: String,
}

/// One entry of the model catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CatalogModel {
    /// Codex's preset identifier (a Claude alias for a Claude Planner), presentation only; never
    /// send it back as a model selection.
    pub id: String,
    /// The slug codex is invoked by, or the Claude alias passed as `--model`.
    pub model: String,
    pub display_name: String,
    pub description: String,
    /// Codex's catalog-level default marker; not "what am I following now". Always `false` for
    /// a Claude alias: the Claude CLI's default is the `null` selection.
    pub is_default: bool,
    /// Empty for a Claude alias: a Claude Planner offers no effort choice.
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    /// The model's own default effort; `null` exactly when the provider offers no effort choice
    /// (a Claude alias). Always a string for a Codex entry.
    #[schema(required = true)]
    pub default_reasoning_effort: Option<String>,
}

impl From<CodexModel> for CatalogModel {
    fn from(m: CodexModel) -> Self {
        Self {
            id: m.id,
            model: m.model,
            display_name: m.display_name,
            description: m.description,
            is_default: m.is_default,
            supported_reasoning_efforts: m
                .supported_reasoning_efforts
                .into_iter()
                .map(|o| ReasoningEffortOption {
                    reasoning_effort: o.reasoning_effort,
                    description: o.description,
                })
                .collect(),
            default_reasoning_effort: Some(m.default_reasoning_effort),
        }
    }
}

impl From<&crate::claude_planner::models::ClaudeModel> for CatalogModel {
    fn from(m: &crate::claude_planner::models::ClaudeModel) -> Self {
        Self {
            id: m.alias.into(),
            model: m.alias.into(),
            display_name: m.display_name.into(),
            description: m.description.into(),
            is_default: false,
            supported_reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
        }
    }
}

/// What a card that has selected nothing currently runs; `null` = not configured anywhere readable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ModelDefaults {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
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
    /// No default could be established: the read failed or no `card_id` was supplied. Always
    /// this for a Claude Planner, whose CLI picks its default model itself.
    Unknown,
}

/// Whether `models` reflects a live catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Codex answered; `models` may still be empty, which is different from "could not ask".
    Live,
    /// The server's fixed list of Claude model aliases (`opus`, `sonnet`, `haiku`), which the
    /// Claude CLI resolves to its current models. Only for a Claude Planner on a server started
    /// with `--claude-planner-config`.
    BuiltIn,
    /// Codex could not be asked, or a Claude Planner is unavailable because the server was started
    /// without `--claude-planner-config`. `models` is empty for lack of an answer.
    Unavailable,
}

/// Response body for `GET /api/models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ModelsResponse {
    pub models: Vec<CatalogModel>,
    pub default: ModelDefaults,
    pub default_source: DefaultSource,
    pub source: ModelSource,
    /// Wall-clock ms at which the catalog was fetched, or `null` when it was not fetched.
    pub fetched_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ModelsQuery {
    /// Resolve the default against this card's workspace; without one the read has no
    /// `cwd` and `default_source` is `unknown`.
    pub card_id: Option<String>,
    /// The Planner provider a card is about to be created with. `claude` (like a `card_id`
    /// naming a Claude Planner card) answers the Claude alias catalog with `source: "built_in"`
    /// without asking Codex, or `source: "unavailable"` with an empty catalog when the server was
    /// started without `--claude-planner-config`.
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
        (status = 200, description = "Model catalog and the default this installation follows. For Codex, answered with `source: \"unavailable\"` and an empty catalog when codex cannot be reached, never with an error and never with a hardcoded catalog. For a Claude Planner, the fixed alias list with `source: \"built_in\"`, or `unavailable` without `--claude-planner-config`", body = ModelsResponse),
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
    // #1810: a Claude Planner's catalog is the alias list, and Codex is not asked. Without the
    // config there is no Claude Planner to choose for, which the picker reads as `unavailable`.
    if q.provider == Some(AgentProvider::Claude)
        || card.as_ref().is_some_and(|card| card.claude_planner)
    {
        let (models, source) = match s.claude_planner.configured() {
            Ok(_) => (
                crate::claude_planner::models::MODELS
                    .iter()
                    .map(CatalogModel::from)
                    .collect(),
                ModelSource::BuiltIn,
            ),
            Err(_) => (Vec::new(), ModelSource::Unavailable),
        };
        return Ok(Json(ModelsResponse {
            models,
            default: ModelDefaults::default(),
            default_source: DefaultSource::Unknown,
            source,
            fetched_at_ms: None,
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
    }
}

/// A card's workspace and whether it is a Claude Planner card.
struct ResolvedCard {
    workspace: String,
    claude_planner: bool,
}

/// The workspace path a card's codex thread runs in — the same value
/// `planner-harness-start` puts on its payload. Any card resolves, not only a planner card.
async fn resolve_card_workspace(s: &RouteState, card_id: &str) -> Result<ResolvedCard> {
    let card = s
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let claude_planner = s.write.verify_role(&card.id).is_some_and(|role| {
        crate::harness::profile::PlannerBinding::from_card(&card, role)
            .is_some_and(|binding| binding.provider == AgentProvider::Claude)
    });
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("track {} for card {card_id}", card.track_id))
        })?;
    Ok(ResolvedCard {
        workspace: track.workspace.path,
        claude_planner,
    })
}
