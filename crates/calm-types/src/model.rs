//! Entity types — the core kernel vocabulary.
//!
//! These are the **only** business-shaped objects the kernel knows about.
//! Everything else (task, calendar, plan, git, doc...) lives in plugins and
//! reaches the kernel through opaque JSON in `Card.payload` or `Overlay.payload`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

pub use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::runtime::{AgentProvider, WorkerSessionKind};
use crate::worker::WorkerSessionState;

// ---------------- CardRole ----------------

/// Authorization role persisted on each card and enforced by `role_gate`.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum CardRole {
    #[default]
    Worker,
    Spec,
    ReportCard,
    /// #1189 — a track-scoped assistant conversation. Reads/writes the
    /// track report through the block channel, runs shell in the track
    /// workspace, and has **no** lifecycle / plan / review / admin
    /// authority. See `role_gate::enforce_assistant_scope`.
    Assistant,
}

impl CardRole {
    pub fn as_db_str(self) -> &'static str {
        match self {
            CardRole::Worker => "worker",
            CardRole::Spec => "spec",
            CardRole::ReportCard => "reportcard",
            CardRole::Assistant => "assistant",
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
            "assistant" => Ok(CardRole::Assistant),
            other => Err(format!("unknown cards.role value `{other}`")),
        }
    }
}

// ---------------- AreaKind ----------------

/// Whether an area is user-visible or kernel-owned storage scaffolding.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum AreaKind {
    #[default]
    User,
    System,
}

impl AreaKind {
    pub fn as_db_str(self) -> &'static str {
        match self {
            AreaKind::User => "user",
            AreaKind::System => "system",
        }
    }
}

impl TryFrom<String> for AreaKind {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "user" => Ok(AreaKind::User),
            "system" => Ok(AreaKind::System),
            other => Err(format!("unknown areas.kind value `{other}`")),
        }
    }
}

// ---------------- Area ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Area {
    #[schema(value_type = String)]
    pub id: AreaId,
    pub name: String,
    pub color: String,
    pub sort: f64,
    #[serde(default)]
    pub kind: AreaKind,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------------- AreaFolder ----------------

/// One row per claimed directory; `path` is absolute and globally
/// unique across the table. A folder transparently covers every
/// descendant path — the kernel resolves a `cwd` to its owning area by
/// finding the claim that covers it (see `GET /api/areas/resolve`).
/// The create endpoint rejects ancestor/descendant overlap with a 409,
/// so at most one claim can cover any given path.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct AreaFolder {
    pub id: i64,
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub path: String,
    pub created_at: i64,
}

/// Issue #250 PR 1 — kind of overlap detected by the
/// `POST /api/areas/:area_id/folders` conflict check. Surfaces in the
/// 409 response body so the frontend can render a precise message
/// without re-parsing strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct FolderConflict {
    pub folder_id: i64,
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub conflict_path: String,
    pub conflict_kind: FolderConflictKind,
}

/// Issue #250 PR 1 — 200 body for `GET /api/areas/resolve`. The
/// resolve endpoint returns `null` (not 404) on miss; this struct is
/// the `Some(_)` payload.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct AreaResolve {
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub folder_id: i64,
    pub folder_path: String,
}

// ---------------- TrackLifecycle ----------------

/// Issue #145 — Track lifecycle state machine.
///
/// One explicit state per track, advanced through a typed state machine
/// (see `crate::track_lifecycle`). The Spec Agent drives the happy path
/// (`draft → planning → dispatching → working → reviewing → done`);
/// the user can cancel any non-terminal state and reopen terminals;
/// worker cards have no authority to touch this field at all.
///
/// **`archived` is intentionally NOT a lifecycle state.** Archive is
/// visibility / history management, orthogonal to execution semantics —
/// a `done`/`failed`/`canceled` track can also be archived without
/// destroying the lifecycle truth. Archival continues to live on the
/// existing `archived_at: Option<i64>` field.
///
/// Persisted as a lowercase string in `tracks.lifecycle` (migration
/// 0012). The serde + sqlx `rename_all = "lowercase"` keeps the wire
/// and storage shape stable; ts-rs exports the matching TS union into
/// `fe/core/api/generated/wire.ts` so the frontend can render the
/// badge against the same vocabulary.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum TrackLifecycle {
    /// New track; user is editing goal/context and hasn't handed off to
    /// the Spec Agent yet. **Default for every newly minted track.**
    #[default]
    Draft,
    /// Spec Agent is reading the goal + code context and producing a plan.
    Planning,
    /// Spec Agent has emitted one or more dispatch requests and the
    /// Dispatcher is spawning worker cards.
    Dispatching,
    /// At least one worker card is executing; the track has not reached
    /// review.
    Working,
    /// Track needs human input, or a worker failed in a way the Spec
    /// Agent cannot recover from autonomously.
    Blocked,
    /// Workers have produced results; Spec Agent or the user is
    /// validating them.
    Reviewing,
    /// Track goal achieved; results accepted. **Terminal.**
    Done,
    /// User chose to abandon the track. **Terminal.**
    Canceled,
    /// System-level failure that cannot recover. **Terminal.**
    Failed,
}

impl TrackLifecycle {
    /// Convenience: is this a terminal state? Terminal states (`done`,
    /// `canceled`, `failed`) cannot transition to anything except via
    /// a user-driven reopen (per `crate::track_lifecycle`).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TrackLifecycle::Done | TrackLifecycle::Canceled | TrackLifecycle::Failed
        )
    }

    /// The lowercase string persisted in `tracks.lifecycle` (migration
    /// 0012). See [`CardRole::as_db_str`] for the sqlx-replacement
    /// rationale.
    pub fn as_db_str(self) -> &'static str {
        match self {
            TrackLifecycle::Draft => "draft",
            TrackLifecycle::Planning => "planning",
            TrackLifecycle::Dispatching => "dispatching",
            TrackLifecycle::Working => "working",
            TrackLifecycle::Blocked => "blocked",
            TrackLifecycle::Reviewing => "reviewing",
            TrackLifecycle::Done => "done",
            TrackLifecycle::Canceled => "canceled",
            TrackLifecycle::Failed => "failed",
        }
    }
}

impl TryFrom<String> for TrackLifecycle {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "draft" => Ok(TrackLifecycle::Draft),
            "planning" => Ok(TrackLifecycle::Planning),
            "dispatching" => Ok(TrackLifecycle::Dispatching),
            "working" => Ok(TrackLifecycle::Working),
            "blocked" => Ok(TrackLifecycle::Blocked),
            "reviewing" => Ok(TrackLifecycle::Reviewing),
            "done" => Ok(TrackLifecycle::Done),
            "canceled" => Ok(TrackLifecycle::Canceled),
            "failed" => Ok(TrackLifecycle::Failed),
            other => Err(format!("unknown tracks.lifecycle value `{other}`")),
        }
    }
}

// ---------------- Track workspace ----------------

/// Ownership must be explicit because only managed workspaces may be recycled.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum TrackWorkspaceKind {
    /// Server-created, exclusively owned, and recyclable.
    Managed,
    /// User-owned; never deleted or initialized by the server.
    #[default]
    Attached,
}

impl TrackWorkspaceKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            TrackWorkspaceKind::Managed => "managed",
            TrackWorkspaceKind::Attached => "attached",
        }
    }
}

impl TryFrom<String> for TrackWorkspaceKind {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        match value.as_str() {
            "managed" => Ok(TrackWorkspaceKind::Managed),
            "attached" => Ok(TrackWorkspaceKind::Attached),
            other => Err(format!("unknown tracks.workspace_kind value `{other}`")),
        }
    }
}

/// A track's typed workspace. `path` is its single stored path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackWorkspace {
    pub kind: TrackWorkspaceKind,
    /// Absolute path.
    pub path: String,
    /// One-shot, monotonic. `Some` ⇒ neither `path` nor `kind` may change
    /// again.
    ///
    /// The system-area launchpad remains unfrozen because it is repointed by
    /// `today_launchpad_ensure_tx`.
    pub frozen_at: Option<i64>,
}

impl Default for TrackWorkspace {
    fn default() -> Self {
        TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: String::new(),
            frozen_at: None,
        }
    }
}

// ---------------- Track ----------------

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Track {
    #[schema(value_type = String)]
    pub id: TrackId,
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub pinned_at: Option<i64>,
    #[serde(default)]
    pub lifecycle: TrackLifecycle,
    /// Wire-compatibility alias of `workspace.path`, serialized as `cwd`.
    ///
    /// Rust readers must use `workspace.path`; this field only preserves the
    /// existing wire shape.
    #[serde(rename = "cwd", default)]
    pub cwd_wire_alias: String,
    /// Template this track was created from.
    ///
    /// The `serde(alias)` below is a deserialization-only compatibility read
    /// for pre-#1209 event-log rows; serialization emits only this name.
    // #1209 PR-2 — this field was renamed, and the alias exists for exactly one
    // carrier: `Track` is `#[serde(flatten)]`-ed into `TrackUpdatedPayload`, so it
    // is embedded verbatim in the immutable event log. Rows written before the
    // rename spell this key with the old name. Without the alias,
    // `#[serde(default)]` would silently replay them as `None` — a lost field
    // with no error. Dropping `default` instead would be worse: deserialization
    // would fail, and `events_since`'s caller of `Event::from_kind_and_payload`
    // logs and *skips the whole row*. So keep BOTH attributes.
    //
    // The asymmetry with `CreateTrackRequest`, which must NOT carry the alias, is
    // deliberate. A request body is a live contract with someone on the other
    // end, so the old spelling there is an observable, fixable 400 via
    // `deny_unknown_fields`. The event log is immutable history, and rejecting
    // it would break replay.
    //
    // Deliberately a non-doc comment: doc comments on this struct are exported
    // into the OpenAPI spec and the ts-rs bindings, and naming the old spelling
    // there would put it back into five generated artifacts.
    #[serde(default, alias = "workflow_id")]
    pub template_id: Option<String>,
    /// Owning plugin copied from the bound template. Immutable after creation.
    #[serde(default)]
    pub plugin_scope: Option<String>,
    /// Server-owned structural marker. Public track creation cannot set this.
    #[serde(default)]
    pub purpose: Option<String>,
    /// Template input is validated at creation and otherwise remains opaque.
    ///
    /// Carries the same deserialization-only alias as `template_id`.
    // #1209 PR-2 — renamed alongside `template_id`; same carrier, same reason,
    // same non-doc comment rationale. See that field.
    #[serde(default, alias = "workflow_input")]
    #[schema(value_type = Option<Object>)]
    #[ts(type = "unknown")]
    pub template_input: Option<serde_json::Value>,
    /// Issue #250 PR 2 — unix-ms timestamp the track most recently
    /// entered a terminal lifecycle state (Done / Canceled / Failed),
    /// or `None` while the track is non-terminal. Stamped inside the
    /// same transaction as the `TrackLifecycleChanged` event by
    /// `track_update_tx`; cleared back to `None` on reopen
    /// (Done/Canceled/Failed → Planning). The calendar window query
    /// `GET /api/tracks?since&until` uses `(terminal_at IS NULL OR
    /// terminal_at >= since)` to keep open tracks visible across every
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
    #[serde(default)]
    pub workspace: TrackWorkspace,
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
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

/// One row of `GET /api/tracks/{track_id}/conversations` (#1189 §4.1).
///
/// Its own type rather than a reuse of [`AreaConversationSummary`], which is
/// what #1189 §6 Q3 leaned towards and what the shapes turned out to require:
/// the area type's contract says "`trackTitle` is absent because every row lives
/// on one hidden track", and on a track that reasoning is simply not true. Two
/// lists with different contracts should not share one name just because their
/// current fields coincide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackConversationSummary {
    /// The assistant card's id. This is the conversation's identity everywhere,
    /// and it is also the card the CARDS panel and `/api/cards/{id}/spec/*`
    /// address.
    pub id: String,
    /// The track this conversation lives on. Always the track in the request
    /// path; carried so a client holding a bare row can navigate.
    pub track_id: String,
    /// The conversation's own name, or null before it has one. Never the
    /// track's title.
    pub title: Option<String>,
    /// Always `"track-assistant"`, derived from the card's persisted marker.
    /// A distinct value from the area list's `"shared-chat"` on purpose: the
    /// frontend branches on it, and a shared value would route assistant rows
    /// through the area chat's presentation.
    pub kind: String,
    /// The live session's state, or **null when the card has no session row**.
    ///
    /// Nullable for the same reason as the area list's: the query LEFT JOINs so
    /// a card whose session is gone (failed start, superseded runtime, shut
    /// down harness) stays visible. Never fill it with an invented value.
    pub state: Option<WorkerSessionState>,
    /// The session's last update, falling back to the card's own.
    pub updated_at: i64,
}

/// One row of `GET /api/areas/{area_id}/conversations` (#1098 §5.5).
///
/// Deliberately absent:
/// * `trackTitle` — every row belongs to the same hidden area chat track, so
///   returning its title would leak an object the user is never shown.
/// * `turns` — the server cannot produce a turn count that agrees with the
///   drawer without re-parsing every `harness_items.params` blob; a number
///   that silently disagrees is worse than no number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct AreaConversationSummary {
    /// The chat card's id. This is the conversation's identity everywhere.
    pub id: String,
    pub track_id: String,
    /// The conversation's own name, or null before it has one. Never the
    /// track's title.
    pub title: Option<String>,
    /// Always `"shared-chat"`, derived from the card's persisted marker rather
    /// than from the session kind (the session is an ordinary codex-card
    /// session and says nothing about the conversation being an area chat).
    pub kind: String,
    /// The live session's state, or **null when the card has no session row**.
    ///
    /// This must stay nullable and must never be filled with an invented
    /// value. The list is a LEFT JOIN precisely so a card whose session is
    /// gone (failed start, superseded runtime, shut-down harness) stays
    /// visible; substituting `idle` or `exited` here would report a session
    /// state that was never read.
    pub state: Option<WorkerSessionState>,
    /// The session's last update, falling back to the card's own — a card
    /// minted seconds ago with no session yet still sorts sensibly.
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Card {
    #[schema(value_type = String)]
    pub id: CardId,
    #[schema(value_type = String)]
    pub track_id: TrackId,
    /// `"terminal"` for built-in PTY cards, `"ui://<plugin>/<view>"` for
    /// plugin-provided cards (the canonical MCP Apps resource URI). The
    /// kernel never interprets beyond that prefix. `[legacy]`
    /// `"plugin:<plugin-id>:<view-id>"` may still appear on persisted rows.
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
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<CardRuntimeView>,
    /// Issue #229 PR A — system-card guard. `true` for user-facing cards
    /// (the default; all pre-#229 rows backfill via the column DEFAULT in
    /// migration 0013). `false` for kernel-owned cards that the user
    /// cannot remove via REST / plugin callbacks — currently spec cards
    /// (retroactively undeletable via the same migration's UPDATE) and
    /// PR B's track-report cards.
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessItem {
    pub id: i64,
    pub runtime_id: String,
    #[schema(value_type = String)]
    pub card_id: CardId,
    #[schema(value_type = String)]
    pub track_id: TrackId,
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
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Overlay {
    pub id: String,
    pub plugin_id: String,
    /// `"track"` or `"card"`.
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
            // Issue #229 PR A — track-report card role. Lowercase, no
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

// ---------------- TrackRecipe (#1292) ----------------

/// A user-defined starting point for a new track.
///
/// A recipe is a saved report: `title` doubles as the report summary, and
/// `body`'s `neige-block` fences **are** its tasks. It is deliberately not a
/// track — #1300 removed "template = a hidden track" because storing recipes
/// that way cost seven "this track is special" exceptions across unrelated
/// subsystems plus a kernel report write that impersonated the user. A
/// recipe row has neither problem: nothing schedules it, nothing lists it
/// among tracks, and every byte in one was written by a human.
///
/// The built-in templates (`calm_server::templates::TEMPLATES`) stay Rust
/// constants and are **not** rows here. Both feed the same instantiation
/// seam, so "built-in" and "mine" differ only in where the payload came
/// from — see `routes::track_recipes`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackRecipe {
    pub id: String,
    /// Picker label *and* the instantiated report's summary. One field, not
    /// two: the three built-in templates already write the same string in
    /// both places, so a second column would not preserve an existing
    /// distinction — it would mint a new way for them to disagree.
    pub title: String,
    /// Report body. Its `neige-block` fences are the tasks.
    pub body: String,
    /// Optimistic-lock anchor. Writers pass the revision they read and the
    /// UPDATE validates + bumps in one statement.
    ///
    /// Deliberately not `updated_at`: a wall clock is not a version. Two
    /// writes inside the same millisecond are indistinguishable by
    /// timestamp, and a clock that steps backwards makes a stale write look
    /// current.
    pub revision: i64,
    pub created_at: i64,
    /// Display only — never a lock anchor. See `revision`.
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTrackRecipe {
    pub title: String,
    pub body: String,
}

#[cfg(test)]
mod area_kind_tests {
    use super::AreaKind;

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        // Wire shape is locked: serde + sqlx storage both emit the
        // lowercase variant name. Migration 0009 stores literal
        // `'user'` / `'system'` strings; changing the rename strategy
        // here would silently desync code-vs-DB.
        for (kind, json) in [
            (AreaKind::User, "\"user\""),
            (AreaKind::System, "\"system\""),
        ] {
            let s = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(s, json, "serialize mismatch for {kind:?}");
            let back: AreaKind = serde_json::from_str(json).expect("deserialize");
            assert_eq!(back, kind, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn default_is_user() {
        assert_eq!(AreaKind::default(), AreaKind::User);
    }

    #[test]
    fn db_str_matches_serde_wire_shape() {
        // See `card_role_tests::db_str_matches_serde_wire_shape`.
        for kind in [AreaKind::User, AreaKind::System] {
            let wire = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(format!("\"{}\"", kind.as_db_str()), wire);
            let back = AreaKind::try_from(kind.as_db_str().to_string()).expect("decode");
            assert_eq!(back, kind);
        }
        assert!(AreaKind::try_from("bogus".to_string()).is_err());
    }
}

#[cfg(test)]
mod track_lifecycle_db_str_tests {
    use super::TrackLifecycle;

    const ALL: [TrackLifecycle; 9] = [
        TrackLifecycle::Draft,
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Blocked,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Done,
        TrackLifecycle::Canceled,
        TrackLifecycle::Failed,
    ];

    #[test]
    fn db_str_matches_serde_wire_shape() {
        // See `card_role_tests::db_str_matches_serde_wire_shape`.
        for state in ALL {
            let wire = serde_json::to_string(&state).expect("serialize");
            assert_eq!(format!("\"{}\"", state.as_db_str()), wire);
            let back = TrackLifecycle::try_from(state.as_db_str().to_string()).expect("decode");
            assert_eq!(back, state);
        }
        assert!(TrackLifecycle::try_from("bogus".to_string()).is_err());
    }
}
