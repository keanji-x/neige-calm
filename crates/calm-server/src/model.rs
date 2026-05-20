//! Entity types — the core kernel vocabulary.
//!
//! These are the **only** business-shaped objects the kernel knows about.
//! Everything else (task, calendar, plan, git, doc...) lives in plugins and
//! reaches the kernel through opaque JSON in `Card.payload` or `Overlay.payload`.
//!
//! Patch structs use `Option<T>` for partial updates: `None` = leave alone,
//! `Some(v)` = replace.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

// ---------------- Cove ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Cove {
    pub id: String,
    pub name: String,
    pub color: String,
    pub sort: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCove {
    pub name: String,
    pub color: String,
    /// If absent, server appends to end.
    pub sort: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct CovePatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub sort: Option<f64>,
}

// ---------------- Wave ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Wave {
    pub id: String,
    pub cove_id: String,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewWave {
    pub cove_id: String,
    pub title: String,
    pub sort: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct WavePatch {
    pub title: Option<String>,
    pub sort: Option<f64>,
    /// Pass `Some(Some(ts))` to archive, `Some(None)` to unarchive,
    /// or omit (`None`) to leave alone.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[schema(value_type = Option<i64>, nullable = true)]
    pub archived_at: Option<Option<i64>>,
}

// ---------------- Card ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Card {
    pub id: String,
    pub wave_id: String,
    /// `"terminal"` for built-in PTY cards, `"plugin:<plugin-id>:<view-id>"`
    /// for plugin-provided cards. Kernel never interprets beyond that prefix.
    pub kind: String,
    pub sort: f64,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    /// Opaque JSON blob — ts-rs would otherwise emit `unknown` via the
    /// `serde-json-impl` feature, but we pin it explicitly so a future
    /// feature-flag change can't silently widen / narrow the surface.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCard {
    /// Defaulted so the REST handler can override from the `:wave_id` path
    /// param without forcing every client body to repeat it. Direct repo
    /// callers must still set this — passing "" produces a NotFound.
    #[serde(default)]
    pub wave_id: String,
    pub kind: String,
    pub sort: Option<f64>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct CardPatch {
    pub kind: Option<String>,
    pub sort: Option<f64>,
    #[schema(value_type = Option<Object>)]
    pub payload: Option<serde_json::Value>,
}

// ---------------- Overlay ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Overlay {
    pub id: String,
    pub plugin_id: String,
    /// `"wave"` or `"card"`.
    pub entity_kind: String,
    pub entity_id: String,
    /// Plugin-defined string. Kernel does not interpret.
    pub kind: String,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    /// Opaque JSON blob — see `Card.payload` for the rationale on the
    /// explicit `unknown` override.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewOverlay {
    pub plugin_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub kind: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

// ---------------- Terminal ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Terminal {
    pub id: String,
    pub card_id: String,
    pub program: String,
    pub cwd: String,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
    pub daemon_handle: Option<String>,
    /// Daemon process id, captured by `spawn_daemon_for` after `cmd.spawn()`.
    /// Used by the orphan-terminal sweeper (`terminal_sweeper`) as the
    /// SIGTERM fallback target when the graceful `ClientMsg::Kill` path
    /// fails. `None` for rows that predate Scope C or for which the spawn
    /// returned no pid (kernel-level edge case).
    pub pid: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewTerminal {
    pub card_id: String,
    pub program: String,
    pub cwd: String,
    #[serde(default = "empty_object")]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
}

// ---------------- Plugin (M3) ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Plugin {
    pub id: String,
    pub version: String,
    pub install_path: String,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    pub manifest: serde_json::Value,
    pub enabled: bool,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    pub user_config: serde_json::Value,
    pub installed_at: i64,
    pub updated_at: i64,
}

/// What `Repo::plugin_install` accepts. `manifest` is the validated JSON blob
/// (see `plugin_host::manifest::Manifest`), `version` is read off the manifest
/// and stored alongside as a denormalized index column.
#[derive(Clone, Debug)]
pub struct NewPlugin {
    pub id: String,
    pub version: String,
    pub install_path: String,
    pub manifest: serde_json::Value,
    /// Plugins land disabled by default. Slice D's enable endpoint flips the
    /// bit. Setting it `true` here is an explicit choice (e.g. seed data,
    /// migration test).
    pub enabled: bool,
    pub user_config: serde_json::Value,
}

// ---------------- Composites ----------------

/// What a Wave detail page renders: the wave itself plus its cards and
/// any overlays scoped to the wave (status/progress badges) and its cards.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct WaveDetail {
    pub wave: Wave,
    pub cards: Vec<Card>,
    pub overlays: Vec<Overlay>,
}

// ---------------- Helpers ----------------

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Deserializes `null` → `Some(None)`, missing → `None`, value → `Some(Some(v))`.
/// Used so `WavePatch.archived_at` can distinguish "leave alone" from "set to null".
fn deserialize_double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(d).map(Some)
}

/// Current unix time in milliseconds — the canonical timestamp the kernel
/// stamps on `*_at` columns.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
