//! `GET /api/models` — the model catalog the planner's model picker reads, plus the
//! default this installation follows. Daemon failures answer 200 with an explicit `source`.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, ErrorBody, Result};
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
    /// Codex's preset identifier, presentation only; never send it back as a model selection.
    pub id: String,
    /// The slug codex is invoked by.
    pub model: String,
    pub display_name: String,
    pub description: String,
    /// Codex's catalog-level default marker; not "what am I following now".
    pub is_default: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: String,
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
            default_reasoning_effort: m.default_reasoning_effort,
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
    /// No default could be established: the read failed or no `card_id` was supplied.
    Unknown,
}

/// Whether `models` reflects a live catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Codex answered; `models` may still be empty, which is different from "could not ask".
    Live,
    /// Codex could not be asked. `models` is empty for lack of an answer.
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
        (status = 200, description = "Model catalog and the default this installation follows. Answered with `source: \"unavailable\"` and an empty catalog when codex cannot be reached, never with an error and never with a hardcoded catalog", body = ModelsResponse),
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
    let cwd = match q
        .card_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(card_id) => Some(resolve_card_workspace(&s, card_id).await?),
        None => None,
    };
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

/// The workspace path a card's codex thread runs in — the same value
/// `planner-harness-start` puts on its payload. Any card resolves, not only a planner card.
async fn resolve_card_workspace(s: &RouteState, card_id: &str) -> Result<String> {
    let card = s
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("track {} for card {card_id}", card.track_id))
        })?;
    Ok(track.workspace.path)
}
