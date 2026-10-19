//! Shared model catalog response contract.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
