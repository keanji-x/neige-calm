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
///   * [`CardRole::ReportCard`] (#229 PR A) is the wave's auto-generated
///     report card. Same kernel-ownership profile as `Spec` — minted by
///     the wave-create path (PR B), one per wave (partial unique index
///     in migration 0013), undeletable from REST / plugin-callback paths.
///     Role-gate-wise it behaves like `Plain`: it only emits `CardUpdated`
///     for its own scope; it does **not** emit `WaveUpdated` (only `Spec`
///     does — preserving the #136 contract).
///
/// Persisted as a lowercase string in `cards.role` (migration 0008). The
/// serde + sqlx `rename_all = "lowercase"` keeps the wire / storage shape
/// stable; ts-rs exports the matching TS union (`"plain" | "spec" |
/// "worker" | "reportcard"`) into `web/src/api/generated-events.ts` so the
/// frontend can adopt the enum once any UI lands.
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
    /// Issue #229 PR A — wave-report card role. See struct docs above
    /// for the kernel-ownership contract. Stored as `"reportcard"`
    /// (lowercase, no hyphen — matches the existing variant naming
    /// convention).
    ReportCard,
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

// ---------------- CoveFolder ----------------

/// Issue #250 PR 1 — a filesystem path claimed by a cove.
///
/// One row per claimed directory; `path` is absolute and globally
/// unique across the table. A folder transparently covers every
/// descendant path — the kernel resolves a `cwd` to its owning cove
/// via longest-prefix matching against this table (see
/// `GET /api/coves/resolve`).
///
/// `id` is an autoincrement integer rather than the kernel's usual
/// uuid-shaped TEXT id because cove_folders is a small, kernel-internal
/// mapping that never appears in the sync engine's event log — there's
/// no replay scenario where two replicas mint divergent ids that must
/// later reconcile. The compact integer also keeps `/folders/:id` URLs
/// readable.
#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CoveFolder {
    pub id: i64,
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub path: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCoveFolder {
    /// Absolute filesystem path. Must start with `/`. The server trims
    /// a trailing slash before insert (root `/` excepted) so equality
    /// and prefix matching stay canonical.
    pub path: String,
}

/// Issue #250 PR 1 — kind of overlap detected by the
/// `POST /api/coves/:cove_id/folders` conflict check. Surfaces in the
/// 409 response body so the frontend can render a precise message
/// without re-parsing strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum FolderConflictKind {
    /// Proposed path equals an existing folder's path exactly.
    Equal,
    /// Proposed path is an ancestor of an existing folder (claiming
    /// `/a` while `/a/b` already exists). Forbidden — would silently
    /// widen the existing claim.
    Ancestor,
    /// Proposed path is a descendant of an existing folder (claiming
    /// `/a/b` while `/a` already exists). Forbidden — the existing
    /// claim already covers it.
    Descendant,
}

/// Issue #250 PR 1 — 409 body for the folder-create conflict case.
/// Hand-written DTO so the frontend gets a structured shape rather
/// than the generic `{error, code}` envelope.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct FolderConflict {
    pub folder_id: i64,
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub conflict_path: String,
    pub conflict_kind: FolderConflictKind,
}

/// Issue #250 PR 1 — 200 body for `GET /api/coves/resolve`. The
/// resolve endpoint returns `null` (not 404) on miss; this struct is
/// the `Some(_)` payload.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CoveResolve {
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub folder_id: i64,
    pub folder_path: String,
}

// ---------------- WaveLifecycle ----------------

/// Issue #145 — Wave lifecycle state machine.
///
/// One explicit state per wave, advanced through a typed state machine
/// (see `crate::wave_lifecycle`). The Spec Agent drives the happy path
/// (`draft → planning → dispatching → working → reviewing → done`);
/// the user can cancel any non-terminal state and reopen terminals;
/// worker cards have no authority to touch this field at all.
///
/// **`archived` is intentionally NOT a lifecycle state.** Archive is
/// visibility / history management, orthogonal to execution semantics —
/// a `done`/`failed`/`canceled` wave can also be archived without
/// destroying the lifecycle truth. Archival continues to live on the
/// existing `archived_at: Option<i64>` field.
///
/// Persisted as a lowercase string in `waves.lifecycle` (migration
/// 0012). The serde + sqlx `rename_all = "lowercase"` keeps the wire
/// and storage shape stable; ts-rs exports the matching TS union into
/// `web/src/api/generated-events.ts` so the frontend can render the
/// badge against the same vocabulary.
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
pub enum WaveLifecycle {
    /// New wave; user is editing goal/context and hasn't handed off to
    /// the Spec Agent yet. **Default for every newly minted wave.**
    #[default]
    Draft,
    /// Spec Agent is reading the goal + code context and producing a plan.
    Planning,
    /// Spec Agent has emitted one or more dispatch requests and the
    /// Dispatcher is spawning worker cards.
    Dispatching,
    /// At least one worker card is executing; the wave has not reached
    /// review.
    Working,
    /// Wave needs human input, or a worker failed in a way the Spec
    /// Agent cannot recover from autonomously.
    Blocked,
    /// Workers have produced results; Spec Agent or the user is
    /// validating them.
    Reviewing,
    /// Wave goal achieved; results accepted. **Terminal.**
    Done,
    /// User chose to abandon the wave. **Terminal.**
    Canceled,
    /// System-level failure that cannot recover. **Terminal.**
    Failed,
}

impl WaveLifecycle {
    /// Convenience: is this a terminal state? Terminal states (`done`,
    /// `canceled`, `failed`) cannot transition to anything except via
    /// a user-driven reopen (per `crate::wave_lifecycle`).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WaveLifecycle::Done | WaveLifecycle::Canceled | WaveLifecycle::Failed
        )
    }
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
    /// Issue #145 — the wave's lifecycle state. **Required** (no
    /// `Option`): every wave-creating code path must seed
    /// [`WaveLifecycle::Draft`] explicitly. Per the project's
    /// "required over Option" preference, Option here would silently
    /// hide missing-data bugs — the field is core kernel contract.
    ///
    /// `#[serde(default)]` lets wire payloads emitted before #145
    /// landed (event-log replay fixtures) parse as `Draft` without
    /// forcing a fixture rewrite — matches the DB DEFAULT in
    /// migration 0012.
    #[serde(default)]
    pub lifecycle: WaveLifecycle,
    /// Issue #250 PR 2 — the working directory the wave's spec daemon
    /// runs in. **Required at the route layer**: `POST /api/waves`
    /// rejects empty / non-absolute paths and refuses to create a wave
    /// whose cwd isn't claimable by some cove (via
    /// `cove_folder_resolve`, optionally creating a `cove_folders` row
    /// when the body sets `attach_folder: true`).
    ///
    /// `#[serde(default)]` mirrors the lifecycle precedent: replay of
    /// a pre-#250 event log fixture (no `cwd` key on `WaveUpdated`)
    /// hydrates as `""`, matching the DB DEFAULT in migration 0016.
    /// Production wave-create paths inside this binary always stamp a
    /// real path — the migration default is the "old data only" fallback.
    #[serde(default)]
    pub cwd: String,
    /// Issue #250 PR 2 — unix-ms timestamp the wave most recently
    /// entered a terminal lifecycle state (Done / Canceled / Failed),
    /// or `None` while the wave is non-terminal. Stamped inside the
    /// same transaction as the `WaveLifecycleChanged` event by
    /// `wave_update_tx`; cleared back to `None` on reopen
    /// (Done/Canceled/Failed → Planning). The calendar window query
    /// `GET /api/waves?since&until` uses `(terminal_at IS NULL OR
    /// terminal_at >= since)` to keep open waves visible across every
    /// day they span.
    ///
    /// Backfill semantics: rows that existed before this migration
    /// stay `None` even when their lifecycle is already terminal —
    /// the event log carries the original transition timestamp but
    /// the migration deliberately doesn't read from `events` (mixing
    /// migration with replay is fragile). A user-driven reopen →
    /// re-Done cycle stamps the column with the current time, which
    /// is the first defensible point.
    #[serde(default)]
    pub terminal_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewWave {
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub title: String,
    pub sort: Option<f64>,
    /// Issue #250 PR 2 — absolute filesystem path the spec daemon will
    /// spawn under. Required (no `Option`): every wave-creating path
    /// must declare a cwd or the spec daemon has no defensible
    /// working directory. The `POST /api/waves` route enforces
    /// absolute-path shape and the cove-folder claim check; the
    /// inner `wave_create_tx` writes whatever the route lands here
    /// verbatim.
    pub cwd: String,
    /// Issue #250 PR 2 — opt-in for "claim this `cwd` for the body's
    /// `cove_id` as a new folder, in the same transaction as the
    /// wave-create write". Default `false`: the cwd must already be
    /// covered by some existing folder under the same cove (the
    /// `cove_folder_resolve` longest-prefix match runs at the route
    /// layer). `true` adds a `cove_folder` row first and then the
    /// wave; folder-conflict rules (equal/ancestor/descendant of any
    /// existing claim) still apply and roll the whole tx back on
    /// conflict.
    #[serde(default)]
    pub attach_folder: bool,
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
    /// Issue #145 — request a lifecycle transition. The actual
    /// transition validation runs through `crate::wave_lifecycle`,
    /// inside the write transaction. Omitting (`None`) means "leave
    /// alone"; `Some(<state>)` triggers the validator against the
    /// (actor, from → to) triple before any DB write or event emit.
    pub lifecycle: Option<WaveLifecycle>,
}

// ---------------- Card ----------------

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Card {
    #[schema(value_type = String)]
    pub id: CardId,
    #[schema(value_type = String)]
    pub wave_id: WaveId,
    /// `"terminal"` for built-in PTY cards, `"ui://<plugin>/<view>"` for
    /// plugin-provided cards (the canonical MCP Apps resource URI). The
    /// kernel never interprets beyond that prefix. `[legacy]`
    /// `"plugin:<plugin-id>:<view-id>"` may also appear on rows persisted
    /// before the M4 cut-over and in server-side perms/manifest enforcement
    /// — see `docs/architecture/terminology-glossary.md` (plugin card kind).
    pub kind: String,
    pub sort: f64,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    /// Opaque JSON blob — ts-rs would otherwise emit `unknown` via the
    /// `serde-json-impl` feature, but we pin it explicitly so a future
    /// feature-flag change can't silently widen / narrow the surface.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    /// Issue #229 PR A — system-card guard. `true` for user-facing cards
    /// (the default; all pre-#229 rows backfill via the column DEFAULT in
    /// migration 0013). `false` for kernel-owned cards that the user
    /// cannot remove via REST / plugin callbacks — currently spec cards
    /// (retroactively undeletable via the same migration's UPDATE) and
    /// PR B's wave-report cards.
    ///
    /// `#[serde(default = "default_deletable")]` so wire payloads emitted
    /// before #229 landed (event-log replay fixtures, old test seeds)
    /// parse as `true` without forcing a fixture rewrite — matches the
    /// DB DEFAULT (1) in migration 0013. The default-fn lives below
    /// because `bool::default()` would give `false` (the *un*safe
    /// fallback for a deny-by-omission auth bit).
    #[serde(default = "default_deletable")]
    pub deletable: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Default for `Card.deletable` when wire payloads / replay fixtures omit
/// the field. Matches the DB DEFAULT in migration 0013 (`1`). See
/// [`Card::deletable`] for the security rationale on biasing the default
/// toward "deletable" rather than `bool::default()`.
fn default_deletable() -> bool {
    true
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
    /// Issue #229 PR A — `deletable` is **not** patchable via API. We
    /// surface it here only so a client sending `{"deletable": ...}`
    /// gets a clear 400 (via the route handler's explicit check) rather
    /// than a silent no-op. `card_update_tx` itself ignores this field
    /// (it never writes the column); the route enforces the rejection
    /// before reaching the txn.
    pub deletable: Option<bool>,
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
            // Issue #229 PR A — wave-report card role. Lowercase, no
            // hyphen, matches the existing variant style. Migration
            // 0013's partial unique index hardcodes the same literal.
            (CardRole::ReportCard, "\"reportcard\""),
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
