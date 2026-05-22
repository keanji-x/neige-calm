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

pub use crate::ids::{ActorId, CardId, CoveId, WaveId};

// ---------------- CardRole ----------------

/// Wave-as-Actor PR3 (#136): authorization label persisted on each card.
///
/// The role decides whether the card's implicit actor (the AI agent bound
/// to it, or the user when no agent is bound) is allowed to emit a given
/// event. The gate is checked at the single write entry — see
/// `role_gate::enforce_role` — *inside* the transaction, before the event
/// row is appended. Violations roll the txn back; nothing is broadcast.
///
///   * [`CardRole::Plain`] is the default for every existing card and
///     every PR3-era card insert. The kernel places no extra restrictions
///     beyond what the wave/cove already provides.
///   * [`CardRole::Spec`] (PR6) is the wave's spec card. Only spec cards
///     may emit `WaveUpdated`; this is the structural choke point that
///     keeps AI workers from rewriting wave-level metadata.
///   * [`CardRole::Worker`] (PR5) is a dispatcher-spawned worker card.
///     Its events are scoped to the card itself and never broaden.
///
/// Persisted as a lowercase string in `cards.role` (migration 0008). The
/// serde + sqlx `rename_all = "lowercase"` keeps the wire / storage shape
/// stable; ts-rs exports the matching TS union (`"plain" | "spec" |
/// "worker"`) into `web/src/api/generated-events.ts` so the frontend can
/// adopt the enum once any UI lands.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    ToSchema,
    TS,
)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum CardRole {
    #[default]
    Plain,
    Spec,
    Worker,
}

// ---------------- CoveKind ----------------

/// Issue #175 — visibility / ownership gate persisted on each cove.
///
/// The kind decides whether the row participates in the user-visible
/// workspace surface (sidebar nav, default `GET /api/coves`) or is an
/// internal kernel-owned entity hidden from the regular UI.
///
///   * [`CoveKind::User`] is the default for every existing cove and
///     every cove minted via `POST /api/coves`. There is no
///     authorization difference from the pre-#175 product — these are
///     the only coves the user ever sees in the sidebar.
///   * [`CoveKind::System`] is a singleton (DB-enforced via a partial
///     unique index in migration 0009) hosting the default Today
///     terminal's wave + card. Created via `cove_create_system_tx`,
///     reachable via the idempotent `POST /api/coves/system` upsert.
///     `GET /api/coves` filters these out by default — opt-in via
///     `?include_system=true`. The user never interacts with this
///     cove directly; it's storage scaffolding, not UI.
///
/// Persisted as a lowercase string in `coves.kind` (migration 0009).
/// The serde + sqlx `rename_all = "lowercase"` keeps the wire / storage
/// shape stable; ts-rs exports the matching TS union
/// (`"user" | "system"`) into `web/src/api/generated-events.ts` so the
/// frontend can validate against it. UI types intentionally don't
/// surface `kind` — the server's default filter already hides system
/// coves, so a one-line `.filter(c => c.kind === 'user')` in CalmApp /
/// router belt-and-suspenders is the only frontend consumer.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    ToSchema,
    TS,
)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum CoveKind {
    #[default]
    User,
    System,
}

// ---------------- Cove ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Cove {
    #[schema(value_type = String)]
    pub id: CoveId,
    pub name: String,
    pub color: String,
    pub sort: f64,
    /// Issue #175 — `User` for sidebar-visible coves, `System` for the
    /// internal singleton that hosts the default Today terminal's wave.
    /// Mirror of `CardRole` precedent on `Card`: persisted at storage
    /// time via DB DEFAULT, never accepted on `POST /api/coves` (which
    /// has no `kind` field — `NewCove` deliberately omits it).
    ///
    /// `#[serde(default)]` so wire payloads emitted before #175 landed
    /// (event-log replay fixtures, old test seeds) parse as `User`
    /// without forcing a fixture rewrite — matches the DB DEFAULT in
    /// migration 0009.
    #[serde(default)]
    pub kind: CoveKind,
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
    #[schema(value_type = String)]
    pub id: WaveId,
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewWave {
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub title: String,
    pub sort: Option<f64>,
    /// Host browser's current theme RGB (#177). When set, the kernel
    /// stamps `--terminal-fg=r,g,b --terminal-bg=r,g,b` onto the auto-
    /// minted spec card's `calm-session-daemon` argv so codex's OSC
    /// 10/11 startup probe gets matching colors. When missing, the
    /// daemon stays silent on OSC queries and codex falls back to its
    /// built-in default — same back-compat as the codex-card endpoint.
    ///
    /// Note: `NewWave` is consumed both inside the
    /// `routes::waves::create_wave` handler (which honors this field)
    /// and by `db::sqlite::wave_create_tx` directly via tests — the
    /// txn-level helper ignores theme since spec-card spawning is
    /// owned by the handler.
    #[serde(default)]
    pub theme: Option<crate::routes::theme::RequestTheme>,
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
    #[schema(value_type = String)]
    pub id: CardId,
    #[schema(value_type = String)]
    pub wave_id: WaveId,
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
    #[schema(value_type = String)]
    pub wave_id: WaveId,
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
    #[schema(value_type = String)]
    pub card_id: CardId,
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
    /// #177 PR2 — host browser's foreground RGB stamped at spawn time,
    /// stored as comma-decimal `r,g,b` matching the daemon CLI's
    /// `--terminal-fg` arg shape. `None` for terminal cards (no theme),
    /// pre-#177 rows, and any spawn path that didn't carry theme. Read
    /// by `spawn_daemon_with_parts` as the fallback when
    /// `SpawnDaemonOpts.terminal_fg` is `None`, closing the WS
    /// auto-revive race where the un-themed shim used to win the
    /// socket against the themed initial spawn.
    pub theme_fg: Option<String>,
    /// Companion to `theme_fg` — host browser's background RGB. Same
    /// shape / lifecycle / fallback semantics.
    pub theme_bg: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewTerminal {
    #[schema(value_type = String)]
    pub card_id: CardId,
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

#[cfg(test)]
mod card_role_tests {
    use super::CardRole;

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        // Wire shape is locked: serde + sqlx storage both emit the
        // lowercase variant name. Migration 0008 inserts the literal
        // `'plain'` string for existing rows; changing the rename
        // strategy here would silently desync code-vs-DB.
        for (role, json) in [
            (CardRole::Plain, "\"plain\""),
            (CardRole::Spec, "\"spec\""),
            (CardRole::Worker, "\"worker\""),
        ] {
            let s = serde_json::to_string(&role).expect("serialize");
            assert_eq!(s, json, "serialize mismatch for {role:?}");
            let back: CardRole = serde_json::from_str(json).expect("deserialize");
            assert_eq!(back, role, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn default_is_plain() {
        assert_eq!(CardRole::default(), CardRole::Plain);
    }
}

#[cfg(test)]
mod cove_kind_tests {
    use super::CoveKind;

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        // Wire shape is locked: serde + sqlx storage both emit the
        // lowercase variant name. Migration 0009 stores literal
        // `'user'` / `'system'` strings; changing the rename strategy
        // here would silently desync code-vs-DB.
        for (kind, json) in [
            (CoveKind::User, "\"user\""),
            (CoveKind::System, "\"system\""),
        ] {
            let s = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(s, json, "serialize mismatch for {kind:?}");
            let back: CoveKind = serde_json::from_str(json).expect("deserialize");
            assert_eq!(back, kind, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn default_is_user() {
        assert_eq!(CoveKind::default(), CoveKind::User);
    }
}
