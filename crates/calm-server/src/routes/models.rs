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
//!
//! That 404 narrows the design's "`GET /api/models` answers 200 in every
//! case" (#1505 design §9, universal statement 4). The narrowing is recorded
//! against that statement in the #1505 issue thread; do not re-derive it from
//! this comment alone.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::codex_appserver::{CodexConfig, CodexModel};
use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, CodexShellState, RouteState};

/// Budget for **the whole request**, not for each codex read inside it.
///
/// One deadline is minted per request and every codex call spends from it, so
/// the number below is the number a reader actually waits. Giving each read
/// its own 8 s timer would have made the endpoint's real worst case 16 s
/// while every comment here still said 8 — the justification below is about
/// how long a person stares at a spinner, so it has to bound the endpoint.
///
/// The per-request client timeout is 30 s and is shared by every RPC the
/// kernel issues, so it cannot be lowered for these calls alone. 30 s already
/// bounds a truly hung daemon; this shorter bound buys responsiveness, not
/// correctness — a picker that spins for half a minute before admitting it
/// has nothing is worse than one that says so in eight seconds. Codex's own
/// hard ceiling on fetching the remote catalog is 5 s
/// (`model-provider/src/models_endpoint.rs`), so 8 s still lets the upstream
/// deadline fire first and answer us.
///
/// The deadline is passed *down* into `CodexAppServer::request_until` rather
/// than wrapped around the call with `tokio::time::timeout`. That is load
/// bearing: the pending-map cleanup lives on the elapse arm inside that
/// function, and cancelling it from outside leaks the entry. See its doc.
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
pub struct CatalogModel {
    /// Codex's *preset* identifier. Presentation only — a React key. It must
    /// never be sent back as a model selection; the slug is
    /// [`CatalogModel::model`].
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
    /// project layers we could resolve.
    ///
    /// Scope note: "we do not substitute a global-layer value for a
    /// per-workspace one" is a property of the **`config_read` branch only**.
    /// `config_toml` reports exactly a global value, deliberately — see its
    /// own doc; it is labelled as a weaker source rather than being reported
    /// **as `config_read`**.
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
    pub models: Vec<CatalogModel>,
    pub default: ModelDefaults,
    pub default_source: DefaultSource,
    pub source: ModelSource,
    /// Wall-clock ms at which the catalog was fetched, or `null` when it was
    /// not fetched at all. There is no server-side cache — codex keeps its own
    /// 300 s disk cache — so this is the age of this response, nothing else.
    pub fetched_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ModelsQuery {
    /// Resolve the default against this card's workspace.
    ///
    /// Config layers are per-directory: a project layer under the card's
    /// workspace can override `model`. Without a card there is no workspace,
    /// so the read is made without a `cwd` and `default_source` is `unknown`
    /// rather than a global-layer value dressed up as this card's default.
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
    // Resolved before anything is asked of codex, and unconditionally: a
    // `card_id` that names no card is a malformed request, and whether it is
    // rejected must not depend on whether a daemon happens to be up. On the
    // dormant path the result then goes unused, which is the intended cost of
    // keeping request validation independent of daemon state.
    //
    // A blank `card_id=` is treated as "no card supplied" rather than as a
    // missing card: it is what a UI sends when nothing is selected, and the
    // empty string is not an id anyone could have been given.
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
        // One arm for every way the catalog can fail to arrive, at `warn!`:
        // reaching it with a live daemon means the picker goes empty while
        // codex is running, which renders identically to a genuine outage and
        // must leave a trace.
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

    // The default source is chosen by whether a CONNECTION exists, never by
    // whether `model/list` happened to succeed. Design §3.4 hangs the choice
    // on the daemon being reachable ("dormant card" ⇒ `config.toml`), and
    // `source` above collapses three different facts into `Unavailable`: no
    // connection, an RPC/decode failure, and a timeout. Deriving the default
    // from `source` would make a live daemon whose catalog we could not read
    // fall back to `config.toml` — the source the design calls the wrong one,
    // because a managed-config layer merges ABOVE the user layer, so that file
    // can name a model this installation does not actually follow.
    let connected = daemon.has_connection().await;
    let (default, default_source) = if connected {
        // The layer-merged read is the only honest answer, and it is only
        // honest for a workspace we were actually given.
        match cwd.as_deref() {
            // Spends what the catalog read left of the same budget.
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
        // Dormant: our own layer is all there is to read. It can be overridden
        // by a managed-config layer, which is exactly why it is labelled as a
        // distinct, weaker source rather than reported as `config_read`.
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

/// A successful `config/read` whose `model` is unset yields
/// `config_read` + `null`, and that is the honest answer — not a hole.
///
/// Codex builds this response by merging the config layers and converting the
/// **effective** `ConfigToml` into the API `Config`
/// (`app-server/src/config_manager_service.rs`, `ConfigManager::read`). A
/// `model` that is absent or null there means no layer sets one, which is
/// precisely the state "this installation follows codex's own built-in
/// default". Reporting `unknown` instead would claim we failed to read
/// something we in fact read completely.
///
/// The failure modes stay distinguishable: codex refusing the read is an RPC
/// error (handled by the caller as `unknown`), and a response missing the
/// `config` member entirely fails to decode — `ConfigReadResponse::config` is
/// not an `Option` — which is also `unknown`. Only a complete read with an
/// unset value lands here.
fn defaults_from_config_read(config: CodexConfig) -> ModelDefaults {
    ModelDefaults {
        model: config.model,
        reasoning_effort: config.model_reasoning_effort,
    }
}

/// The workspace path a card's codex thread runs in.
///
/// This is the same value `planner-harness-start` puts on its payload
/// (`track.workspace.path`), which travels unchanged to
/// `SharedThreadStartParams.cwd`, so `config/read` folds in exactly the
/// project layers that thread sees.
///
/// That equality holds over time only because **re-pointing a track's
/// workspace forces a new thread** rather than moving a live one — the
/// reasoning for that lives in `routes/today.rs` (the legacy-Today adoption
/// path, which deliberately invalidates the old planner thread when it
/// repurposes the workspace). If that contract ever changes, this function's
/// claim to name "the thread's cwd" changes with it.
///
/// **Any card resolves**, not only a planner card: there is no role or kind
/// check here. A terminal/worker/report card id answers with its track's
/// workspace default, which is a truthful answer to "what would a codex
/// thread in this track follow" and leaks nothing the caller's session does
/// not already grant. The parameter is documented as a card because that is
/// how the picker addresses it, not because the handler narrows to one kind.
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
