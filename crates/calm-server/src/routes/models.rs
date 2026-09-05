//! `GET /api/models` — the model catalog the planner's model picker reads,
//! plus the default this installation would follow if a card picks nothing.
//!
//! ## Two independent answers in one response
//!
//! * `models` + `source` answer *"what can be chosen"*. They come from the
//!   shared codex daemon's `model/list`.
//! * `default` + `default_source` answer *"what runs when a card chooses
//!   nothing"*. They come from `config/read` when a daemon connection exists
//!   and from the shared `config.toml` when it does not.
//!
//! The two are deliberately not collapsed. Codex's own `isDefault` flag marks
//! *the entry the picker highlights* (`manager.rs`'s
//! `mark_default_by_picker_visibility`: the first picker-visible preset, else
//! the first preset). It is not the value this installation follows, so it
//! must never be used to fill `default`.
//!
//! ## This endpoint never fails on the daemon's account
//!
//! No daemon connection, an RPC error, a timeout, or a catalog codex answers
//! as empty all return **200** with an explicit `source`. That is the whole
//! point: a picker that 500s teaches the reader nothing, and a hardcoded
//! fallback catalog would let a card be sent a slug this account cannot run.
//! The daemon is never spawned or healed to serve a GET.
//!
//! The 200 guarantee covers the daemon branches only. It does not cover the
//! session gate (`require_session` answers 401 above this handler), and it
//! does not cover a `card_id` naming a card that does not exist — that is a
//! malformed request, not an unavailable daemon, and it answers 404.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, CodexShellState, RouteState};

/// Call-site bound on the two codex reads this endpoint makes.
///
/// The per-request client timeout is 30 s and is shared by every RPC the
/// kernel issues, so it cannot be lowered for these two alone. 30 s already
/// bounds a truly hung daemon; this shorter bound buys responsiveness, not
/// correctness — a picker that spins for half a minute before admitting it
/// has nothing is worse than one that says so in eight seconds. Codex's own
/// hard ceiling on fetching the remote catalog is 5 s
/// (`model-provider/src/models_endpoint.rs`), so 8 s still lets the upstream
/// deadline fire first and answer us.
pub const CODEX_READ_TIMEOUT: Duration = Duration::from_secs(8);

/// One selectable reasoning effort for a model. `description` is codex's own
/// copy, passed through verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReasoningEffortOption {
    /// A bare string, never a closed enum: codex's `ReasoningEffort` carries a
    /// `Custom(String)` variant and accepts any non-empty string on the wire.
    pub reasoning_effort: String,
    pub description: String,
}

/// One entry of the model catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Model {
    /// Codex's *preset* identifier. Presentation only — a React key. It must
    /// never be sent back as a model selection; the slug is [`Model::model`].
    pub id: String,
    /// The slug codex is invoked by. This is the value that travels to
    /// `turn/start`, into `cards.payload_json`, and in a selection request.
    pub model: String,
    pub display_name: String,
    pub description: String,
    /// Codex's catalog-level default marker. Useful for ordering and
    /// highlighting the list; **not** an answer to "what am I following now".
    pub is_default: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: String,
}

impl From<CodexModel> for Model {
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

/// What a card that has selected nothing currently runs. `null` means "not
/// configured anywhere we could read".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ModelDefaults {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// Where [`ModelsResponse::default`] came from. Always present, including
/// when the catalog is unavailable — the picker's own disabled state depends
/// on being told which answer it is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// The daemon's layer-merged effective config, read for the requested
    /// card's workspace.
    ConfigRead,
    /// The shared CODEX_HOME `config.toml`, read directly because no daemon
    /// connection exists. Weaker than `config_read`: it reports our own layer,
    /// which a managed-config layer can override.
    ConfigToml,
    /// We could not establish which default applies. Either the read failed,
    /// or no `card_id` was supplied and there is therefore no workspace whose
    /// project layers we could resolve. A global-layer value is **not**
    /// reported in place of a per-workspace one.
    Unknown,
}

/// Whether `models` reflects a live catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Codex answered. `models` may still be empty — that means this account
    /// has no selectable models, which is a different fact from "we could not
    /// ask", and the reader must be able to tell them apart.
    Live,
    /// Codex could not be asked. `models` is empty for lack of an answer.
    Unavailable,
}

/// Response body for `GET /api/models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ModelsResponse {
    pub models: Vec<Model>,
    pub default: ModelDefaults,
    pub default_source: DefaultSource,
    pub source: ModelSource,
    /// Wall-clock ms at which the catalog was fetched, or `null` when it was
    /// not fetched at all. There is no server-side cache — codex keeps its own
    /// 300 s disk cache — so this is the age of this response, nothing else.
    pub fetched_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct ModelsQuery {
    /// Resolve the default against this card's workspace.
    ///
    /// Config layers are per-directory: a project layer under the card's
    /// workspace can override `model`. Without a card there is no workspace,
    /// so the read is made without a `cwd` and `default_source` is `unknown`
    /// rather than a global-layer value dressed up as this card's default.
    #[serde(default)]
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
    let cwd = match q.card_id.as_deref() {
        Some(card_id) => Some(resolve_card_workspace(&s, card_id).await?),
        None => None,
    };
    let daemon = &codex.shared_codex_appserver;

    let listed = match tokio::time::timeout(CODEX_READ_TIMEOUT, daemon.model_list()).await {
        Ok(Ok(models)) => Some(models),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "GET /api/models: model/list unavailable");
            None
        }
        Err(_elapsed) => {
            tracing::warn!(
                timeout_secs = CODEX_READ_TIMEOUT.as_secs(),
                "GET /api/models: model/list timed out"
            );
            None
        }
    };

    let (models, source, fetched_at_ms) = match listed {
        Some(models) => (
            models.into_iter().map(Model::from).collect(),
            ModelSource::Live,
            Some(crate::model::now_ms()),
        ),
        None => (Vec::new(), ModelSource::Unavailable, None),
    };

    let (default, default_source) = match source {
        // A live connection: the layer-merged read is the only honest answer,
        // and it is only honest for a workspace we were actually given.
        ModelSource::Live => match cwd.as_deref() {
            Some(cwd) => {
                match tokio::time::timeout(CODEX_READ_TIMEOUT, daemon.config_read(Some(cwd))).await
                {
                    Ok(Ok(config)) => {
                        (defaults_from_config_read(config), DefaultSource::ConfigRead)
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "GET /api/models: config/read failed");
                        (ModelDefaults::default(), DefaultSource::Unknown)
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = CODEX_READ_TIMEOUT.as_secs(),
                            "GET /api/models: config/read timed out"
                        );
                        (ModelDefaults::default(), DefaultSource::Unknown)
                    }
                }
            }
            None => (ModelDefaults::default(), DefaultSource::Unknown),
        },
        // Dormant: our own layer is all there is to read. It can be overridden
        // by a managed-config layer, which is exactly why it is labelled as a
        // distinct, weaker source rather than reported as `config_read`.
        ModelSource::Unavailable => match daemon.shared_home().read_default_model_settings() {
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
        },
    };

    Ok(Json(ModelsResponse {
        models,
        default,
        default_source,
        source,
        fetched_at_ms,
    }))
}

fn defaults_from_config_read(config: CodexConfig) -> ModelDefaults {
    ModelDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
    }
}

/// The workspace path a card's codex thread runs in.
///
/// This is the same value `planner-harness-start` puts on its payload
/// (`track.workspace.path`) and therefore the same `cwd` `thread/start` used,
/// so `config/read` folds in exactly the project layers that thread sees.
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
