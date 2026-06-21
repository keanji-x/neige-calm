//! Entity types — the core kernel vocabulary.
//!
//! These are the **only** business-shaped objects the kernel knows about.
//! Everything else (task, calendar, plan, git, doc...) lives in plugins and
//! reaches the kernel through opaque JSON in `Card.payload` or `Overlay.payload`.
//!
//! ## #679 PR1 — what lives here vs calm-server
//!
//! This module holds the IO-free entity/DTO vocabulary that the frontend's
//! generated TS bindings are derived from. Route-coupled request DTOs
//! (`NewWave` carries a `RequestTheme`, patch structs, `NewCove`/`NewCard`…)
//! and the sqlx-only entities (`Terminal`, `Plugin`, `Task`) stay in
//! calm-server's `model.rs`, which re-exports everything here so
//! `calm_server::model::*` paths are unchanged.
//!
//! Because calm-types carries no sqlx, the entities here no longer derive
//! `sqlx::FromRow` / `sqlx::Type`. Row mapping lives in calm-server's
//! `db::rows` wrappers (PR2 moves it to calm-truth); the persisted TEXT
//! shapes are pinned by the `as_db_str` / `TryFrom<String>` impls below and
//! their tests.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

pub use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::runtime::{AgentProvider, WorkerSessionKind};
use crate::worker::WorkerSessionState;

// ---------------- CardRole ----------------

/// Wave-as-Actor PR3 (#136): authorization label persisted on each card.
///
/// The role decides whether the card's implicit actor (the AI agent bound
/// to it, or the user when no agent is bound) is allowed to emit a given
/// event. The gate is checked at the single write entry — see
/// `role_gate::enforce_role` — *inside* the transaction, before the event
/// row is appended. Violations roll the txn back; nothing is broadcast.
///
///   * [`CardRole::Spec`] (PR6) is the wave's spec card. Only spec cards
///     may emit `WaveUpdated`; this is the structural choke point that
///     keeps AI workers from rewriting wave-level metadata.
///   * [`CardRole::Worker`] is the default for user-facing card inserts
///     and dispatcher-spawned worker cards. Its events are scoped to the
///     card itself and never broaden.
///   * [`CardRole::ReportCard`] (#229 PR A) is the wave's auto-generated
///     report card. Same kernel-ownership profile as `Spec` — minted by
///     the wave-create path (PR B), one per wave (partial unique index
///     in migration 0013), undeletable from REST / plugin-callback paths.
///     Role-gate-wise it behaves like `Worker`: it only emits `CardUpdated`
///     for its own scope; it does **not** emit `WaveUpdated` (only `Spec`
///     does — preserving the #136 contract).
///
/// Persisted as a lowercase string in `cards.role` (migration 0008). The
/// serde + sqlx `rename_all = "lowercase"` keeps the wire / storage shape
/// stable; ts-rs exports the matching TS union (`"spec" | "worker" |
/// "reportcard"`) into `web/src/api/generated-events.ts` so the
/// frontend can adopt the enum once any UI lands.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum CardRole {
    #[default]
    Worker,
    Spec,
    /// Issue #229 PR A — wave-report card role. See struct docs above
    /// for the kernel-ownership contract. Stored as `"reportcard"`
    /// (lowercase, no hyphen — matches the existing variant naming
    /// convention).
    ReportCard,
}

impl CardRole {
    /// The lowercase string persisted in `cards.role` (migration 0008).
    /// Replaces the `sqlx::Type` derive the enum carried while it lived in
    /// calm-server — bind sites pass this, decode goes through
    /// [`TryFrom<String>`].
    pub fn as_db_str(self) -> &'static str {
        match self {
            CardRole::Worker => "worker",
            CardRole::Spec => "spec",
            CardRole::ReportCard => "reportcard",
        }
    }
}

impl TryFrom<String> for CardRole {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "worker" => Ok(CardRole::Worker),
            "spec" => Ok(CardRole::Spec),
            "reportcard" => Ok(CardRole::ReportCard),
            other => Err(format!("unknown cards.role value `{other}`")),
        }
    }
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
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum CoveKind {
    #[default]
    User,
    System,
}

impl CoveKind {
    /// The lowercase string persisted in `coves.kind` (migration 0009).
    /// See [`CardRole::as_db_str`] for the sqlx-replacement rationale.
    pub fn as_db_str(self) -> &'static str {
        match self {
            CoveKind::User => "user",
            CoveKind::System => "system",
        }
    }
}

impl TryFrom<String> for CoveKind {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "user" => Ok(CoveKind::User),
            "system" => Ok(CoveKind::System),
            other => Err(format!("unknown coves.kind value `{other}`")),
        }
    }
}

// ---------------- Cove ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
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
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CoveFolder {
    pub id: i64,
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub path: String,
    pub created_at: i64,
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
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
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

    /// The lowercase string persisted in `waves.lifecycle` (migration
    /// 0012). See [`CardRole::as_db_str`] for the sqlx-replacement
    /// rationale.
    pub fn as_db_str(self) -> &'static str {
        match self {
            WaveLifecycle::Draft => "draft",
            WaveLifecycle::Planning => "planning",
            WaveLifecycle::Dispatching => "dispatching",
            WaveLifecycle::Working => "working",
            WaveLifecycle::Blocked => "blocked",
            WaveLifecycle::Reviewing => "reviewing",
            WaveLifecycle::Done => "done",
            WaveLifecycle::Canceled => "canceled",
            WaveLifecycle::Failed => "failed",
        }
    }
}

impl TryFrom<String> for WaveLifecycle {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "draft" => Ok(WaveLifecycle::Draft),
            "planning" => Ok(WaveLifecycle::Planning),
            "dispatching" => Ok(WaveLifecycle::Dispatching),
            "working" => Ok(WaveLifecycle::Working),
            "blocked" => Ok(WaveLifecycle::Blocked),
            "reviewing" => Ok(WaveLifecycle::Reviewing),
            "done" => Ok(WaveLifecycle::Done),
            "canceled" => Ok(WaveLifecycle::Canceled),
            "failed" => Ok(WaveLifecycle::Failed),
            other => Err(format!("unknown waves.lifecycle value `{other}`")),
        }
    }
}

// ---------------- Wave ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Wave {
    #[schema(value_type = String)]
    pub id: WaveId,
    #[schema(value_type = String)]
    pub cove_id: CoveId,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub pinned_at: Option<i64>,
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
    /// hydrates as `""`, matching the DB DEFAULT in migration 0018.
    /// Production wave-create paths inside this binary always stamp a
    /// real path — the migration default is the "old data only" fallback.
    #[serde(default)]
    pub cwd: String,
    pub workflow_id: Option<String>,
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

// ---------------- Card ----------------

/// Live runtime projection read from `worker_sessions` when a card is fetched
/// or serialized.
///
/// This view is not part of the idempotency contract: across retries the
/// worker session may have advanced, so `Card.runtime` may differ between the
/// first POST response and a retry POST response returning the same operation
/// result. Future cleanup (#581 item 4) will remove the legacy payload-key
/// projection; this typed view is the forward-compatible reader path.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CardRuntimeView {
    pub runtime_id: String,
    pub kind: WorkerSessionKind,
    pub status: WorkerSessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<AgentProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
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
    #[schema(value_type = Object)]
    /// Opaque JSON blob — ts-rs would otherwise emit `unknown` via the
    /// `serde-json-impl` feature, but we pin it explicitly so a future
    /// feature-flag change can't silently widen / narrow the surface.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<CardRuntimeView>,
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
pub fn default_deletable() -> bool {
    true
}

// ---------------- HarnessItem ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct HarnessItem {
    pub id: i64,
    pub runtime_id: String,
    #[schema(value_type = String)]
    pub card_id: CardId,
    #[schema(value_type = String)]
    pub wave_id: WaveId,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub item_uuid: Option<String>,
    pub item_type: Option<String>,
    pub method: String,
    pub params: String,
    pub created_at_ms: i64,
}

// ---------------- Overlay ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct Overlay {
    pub id: String,
    pub plugin_id: String,
    /// `"wave"` or `"card"`.
    pub entity_kind: String,
    pub entity_id: String,
    /// Plugin-defined string. Kernel does not interpret.
    pub kind: String,
    #[schema(value_type = Object)]
    /// Opaque JSON blob — see `Card.payload` for the rationale on the
    /// explicit `unknown` override.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    pub updated_at: i64,
}

#[cfg(test)]
mod card_role_tests {
    use super::CardRole;

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        // Wire shape is locked: serde + sqlx storage both emit the
        // lowercase variant name. Changing the rename strategy here would
        // silently desync code-vs-DB.
        for (role, json) in [
            (CardRole::Worker, "\"worker\""),
            (CardRole::Spec, "\"spec\""),
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
    fn default_is_worker() {
        assert_eq!(CardRole::default(), CardRole::Worker);
    }

    #[test]
    fn db_str_matches_serde_wire_shape() {
        // `as_db_str` replaces the `#[sqlx(rename_all = "lowercase")]`
        // derive the enum carried in calm-server. Pin the DB string to the
        // serde wire string so the storage shape can't silently drift from
        // the wire shape (#679 PR1).
        for role in [CardRole::Worker, CardRole::Spec, CardRole::ReportCard] {
            let wire = serde_json::to_string(&role).expect("serialize");
            assert_eq!(format!("\"{}\"", role.as_db_str()), wire);
            let back = CardRole::try_from(role.as_db_str().to_string()).expect("decode");
            assert_eq!(back, role);
        }
        assert!(CardRole::try_from("bogus".to_string()).is_err());
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

    #[test]
    fn db_str_matches_serde_wire_shape() {
        // See `card_role_tests::db_str_matches_serde_wire_shape`.
        for kind in [CoveKind::User, CoveKind::System] {
            let wire = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(format!("\"{}\"", kind.as_db_str()), wire);
            let back = CoveKind::try_from(kind.as_db_str().to_string()).expect("decode");
            assert_eq!(back, kind);
        }
        assert!(CoveKind::try_from("bogus".to_string()).is_err());
    }
}

#[cfg(test)]
mod wave_lifecycle_db_str_tests {
    use super::WaveLifecycle;

    const ALL: [WaveLifecycle; 9] = [
        WaveLifecycle::Draft,
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Blocked,
        WaveLifecycle::Reviewing,
        WaveLifecycle::Done,
        WaveLifecycle::Canceled,
        WaveLifecycle::Failed,
    ];

    #[test]
    fn db_str_matches_serde_wire_shape() {
        // See `card_role_tests::db_str_matches_serde_wire_shape`.
        for state in ALL {
            let wire = serde_json::to_string(&state).expect("serialize");
            assert_eq!(format!("\"{}\"", state.as_db_str()), wire);
            let back = WaveLifecycle::try_from(state.as_db_str().to_string()).expect("decode");
            assert_eq!(back, state);
        }
        assert!(WaveLifecycle::try_from("bogus".to_string()).is_err());
    }
}
