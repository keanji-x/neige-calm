//! Entity types — the core kernel vocabulary.
//!
//! #679 PR1: the IO-free entity/DTO vocabulary moved to `calm-types`
//! (`calm_types::model`) and is re-exported below, so every existing
//! `crate::model::Area` / `calm_server::model::Card` path keeps working.
//! What stays defined here:
//!
//!   * route-coupled request DTOs (`NewTrack` / `NewTerminal` carry a
//!     `RequestTheme`; the `New*`/`*Patch` family is REST surface, not
//!     vocabulary);
//!   * sqlx-coupled entities with no TS export (`Terminal`, `Plugin`,
//!     `Task` + its enums) — they keep their `sqlx::FromRow`/`sqlx::Type`
//!     derives, which calm-types cannot host (zero-IO rule). Row mapping
//!     for the *moved* entities lives in `crate::db::rows`;
//!   * the `now_ms` / `new_id` helpers (uuid stays a calm-server dep).
//!
//! Patch structs use `Option<T>` for partial updates: `None` = leave alone,
//! `Some(v)` = replace.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use crate::ids::{ActorId, AreaId, CardId, TrackId};
// #679 PR1 — moved vocabulary, re-exported at the old paths. The source
// definitions live in calm-types; do NOT re-declare them here (shim-window
// type-drift risk, issue #679 "Greenfield-specific risks" #4).
pub use calm_types::model::{
    Area, AreaFolder, AreaKind, AreaResolve, Card, CardRole, CardRuntimeView, FolderConflict,
    FolderConflictKind, HarnessInputPresentation, HarnessInputSegment, HarnessItem, NewTrackRecipe,
    Overlay, Track, TrackConversationSummary, TrackLifecycle, TrackRecipe, TrackWorkspace,
    TrackWorkspaceKind, default_deletable,
};

/// Wire shape of `NewCodexCardBody.theme` / `NewTrack.theme`. Matches the
/// `calm_session::TerminalTheme` value type one-for-one — duplicated
/// here so the route can keep its own `ToSchema` derive (the
/// `calm_session` crate is utoipa-free).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestTheme {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

impl RequestTheme {
    pub fn fg_arg(&self) -> String {
        let (r, g, b) = self.fg;
        format!("{r},{g},{b}")
    }

    pub fn bg_arg(&self) -> String {
        let (r, g, b) = self.bg;
        format!("{r},{g},{b}")
    }

    pub fn default_dark() -> Self {
        Self {
            fg: (216, 219, 226),
            bg: (15, 20, 24),
        }
    }
}

// ---------------- Area DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewArea {
    pub name: String,
    pub color: String,
    /// If absent, server appends to end.
    pub sort: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct AreaPatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub sort: Option<f64>,
    /// Missing leaves the preference alone, null clears it, and a string sets
    /// the built-in template preselected by the New Track surface.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub default_template_id: Option<Option<String>>,
    /// Missing leaves the preference alone, null restores managed workspaces,
    /// and a string sets the exact attached working directory to preselect.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub default_cwd: Option<Option<String>>,
}

// ---------------- AreaFolder DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewAreaFolder {
    /// Absolute filesystem path. Must start with `/`. The server trims
    /// a trailing slash before insert (root `/` excepted) so equality
    /// and prefix matching stay canonical.
    pub path: String,
}

// ---------------- Track DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTrack {
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub title: String,
    pub sort: Option<f64>,
    /// Issue #250 PR 2 — absolute filesystem path the planner daemon will
    /// spawn under. Required (no `Option`): every track-creating path
    /// must declare a cwd or the planner daemon has no defensible
    /// working directory. The `POST /api/tracks` route enforces
    /// absolute-path shape and the area-folder claim check; the
    /// inner `track_create_tx` writes whatever the route lands here
    /// verbatim.
    pub cwd: String,
    #[serde(default)]
    pub template_id: Option<String>,
    /// #1110 S4 — copied from the owning Manifest at create. Not accepted on
    /// `POST /api/tracks` (CreateTrackRequest deny_unknown_fields); the route
    /// stamps it from the resolved trusted plugin. `#[serde(default)]` keeps
    /// direct repo callers additive under `deny_unknown_fields`.
    #[serde(default)]
    pub plugin_scope: Option<String>,
    /// Issue #891 / #1110 S2 — JSON input for the bound template. Only
    /// accepted when `template_id` names a template a running trusted plugin
    /// binds to and whose Manifest declares an `input_schema`; the `POST /api/tracks`
    /// route validates the value against that schema before any DB write. The
    /// kernel never interprets the blob — it is persisted verbatim and injected
    /// into the planner harness developer instructions at thread-mint time.
    /// `#[serde(default)]` keeps the field purely additive under
    /// `deny_unknown_fields`.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub template_input: Option<serde_json::Value>,
    /// Issue #250 PR 2 — opt-in for "claim this `cwd` for the body's
    /// `area_id` as a new folder, in the same transaction as the
    /// track-create write". Default `false`: the cwd must already be
    /// covered by some existing folder under the same area. Both the
    /// covering scan and the claim insert run inside that one
    /// transaction (issue #275), through the same
    /// [`crate::area_folder_claim::find_owner`] rule
    /// `GET /api/areas/resolve` uses. `true` adds a `area_folder` row
    /// first and then the track; folder-conflict rules
    /// (equal/ancestor/descendant of any existing claim) still apply and
    /// roll the whole tx back on conflict.
    #[serde(default)]
    pub attach_folder: bool,
    /// Host browser's current theme RGB (#177). Required end-to-end so
    /// the auto-minted planner card's terminal renderer answers codex's
    /// OSC 10/11 startup probe with matching colors. A body
    /// missing this field is rejected at the deserialize layer (422):
    /// the planner card is invisible to the user and a silent fallback
    /// would mean every track-from-the-UI spawned with a mis-tinted
    /// composer (the bug that motivated this refactor).
    ///
    /// Direct repo callers (`db::sqlite::track_create_tx`, used by tests
    /// and a couple of non-route helpers) still pass a value here even
    /// though the txn-level helper does not consume it — planner-card
    /// spawning is owned by `routes::tracks::create_track`. Tests can
    /// use `RequestTheme::default_dark()` as a no-op sentinel.
    pub theme: RequestTheme,
}

/// INV-1110-004: `plugin_scope` is create-time only and is not a field here.
/// PATCH `/api/tracks` cannot widen or change it. Extra JSON keys are ignored
/// (`deny_unknown_fields` is not set).
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct TrackPatch {
    pub title: Option<String>,
    pub sort: Option<f64>,
    /// Pass `Some(Some(ts))` to archive, `Some(None)` to unarchive,
    /// or omit (`None`) to leave alone.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub archived_at: Option<Option<i64>>,
    /// Pass `Some(Some(ts))` to pin, `Some(None)` to unpin,
    /// or omit (`None`) to leave alone.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub pinned_at: Option<Option<i64>>,
    /// Issue #145 — request a lifecycle transition. The actual
    /// transition validation runs through `crate::track_lifecycle`,
    /// inside the write transaction. Omitting (`None`) means "leave
    /// alone"; `Some(<state>)` triggers the validator against the
    /// (actor, from → to) triple before any DB write or event emit.
    pub lifecycle: Option<TrackLifecycle>,
    /// Issue #644 — per-track scheduler budget (`tracks.task_budget`,
    /// migration 0041). Pass `Some(Some(n))` to set, `Some(None)` to
    /// clear back to the kernel default, or omit (`None`) to leave
    /// alone. Inert until the PR-B scheduler reads it.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub task_budget: Option<Option<i64>>,
    /// Issue #985 — maximum admitted planner-declared task inventory. A
    /// present null resets to the kernel default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub planner_task_ceiling: Option<Option<i64>>,
    /// Issue #985 — per-track declaration policy. A present null resets to
    /// the kernel default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub automation_policy: Option<Option<String>>,
    /// Issue #985 slice 6 PR-B — budget for the non-terminal planner inventory of
    /// the WHOLE track tree. Root-only: `track_update_tx` refuses the patch on a
    /// track with a parent, since a per-child budget would make the tree bound
    /// vacuous. A present null resets to the kernel default (32).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub tree_task_budget: Option<Option<i64>>,
    /// Issue #644 — track-level gate policy (`tracks.require_task_gates`,
    /// migration 0041). `Some(v)` sets the flag, omit to leave alone.
    /// Enforced by `calm.plan.upsert` rule 6 only from PR-C onward.
    pub require_task_gates: Option<bool>,
    /// #1147 S3 — request a workspace change (design §更换与冻结).
    ///
    /// Handled entirely by `routes::tracks::update_track` and **never** by
    /// `track_update_tx`: a re-point is a filesystem move bracketed by two
    /// transactions, not a column write, so there is nothing here for the
    /// mechanical row writer to apply. It is also mutually exclusive with
    /// every other field in this struct — see the route.
    #[serde(default)]
    pub workspace: Option<TrackWorkspacePatch>,
}

/// #1147 S3 — point a track at a repository the user already has.
///
/// The only transition this expresses is `managed → attached`. There is no
/// `managed → managed`: a managed path is *derived*
/// (`<workspace-root>/<area_id>/<track_id>`, see
/// `workspace_materialize::managed_workspace_path`) from a track's area and id,
/// neither of which can change, so "re-allocate a managed workspace" would
/// always re-derive the same path — an in-place reset, not a change. And a
/// caller-supplied *managed* path is worse than useless: S5's recycle guard 2
/// requires exactly `<root>/<area>/<track>` depth, so any other path produces a
/// row whose directory can never be reclaimed.
///
/// `attached → *` stays refused (an attached repository belongs to the user;
/// the server never moves, initializes or deletes it), which makes this a
/// one-way door — and the write below stamps `frozen_at` to say so.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackWorkspacePatch {
    /// Must be `attached`. `managed` is a documented 400, not a silent no-op.
    pub kind: TrackWorkspaceKind,
    /// Absolute path to an existing Git work tree. Validated — existence and
    /// git-ness included — *before* anything is written, because "the path was
    /// wrong" surfacing later as a worker's `spawn-failed` is the defect
    /// #1147 was opened on.
    pub path: String,
    /// Claim `path` for this track's area in the same transaction, exactly as
    /// `POST /api/tracks`'s field of the same name does (issue #275 rules:
    /// equal / ancestor / descendant of any existing claim is a structured
    /// 409). Default `false`: an unclaimed path is refused rather than
    /// silently making a homeless track.
    #[serde(default)]
    pub attach_folder: bool,
}

// ---------------- Card DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCard {
    /// Defaulted so the REST handler can override from the `:track_id` path
    /// param without forcing every client body to repeat it. Direct repo
    /// callers must still set this — passing "" produces a NotFound.
    #[serde(default)]
    #[schema(value_type = String)]
    pub track_id: TrackId,
    pub kind: String,
    pub sort: Option<f64>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct CardPatch {
    pub title: Option<String>,
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

// ---------------- Overlay DTOs ----------------

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
    #[sqlx(try_from = "String")]
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub program: String,
    pub cwd: String,
    #[sqlx(json)]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
    /// Child process id, captured after supervisor spawn. Used by the
    /// orphan-terminal sweeper (`terminal_sweeper`) as the SIGTERM fallback
    /// target. `None` for rows that predate Scope C or for which the spawn
    /// returned no pid (kernel-level edge case).
    pub pid: Option<i64>,
    /// #177 — host browser's foreground RGB at row-creation time, as
    /// comma-decimal `r,g,b` format). NOT NULL after migration 0017:
    /// every spawn path reads these columns so renderer startup observes
    /// the browser theme.
    pub theme_fg: String,
    /// #177 — host browser's background RGB at row-creation time.
    /// Mirrors `theme_fg` semantics; both columns are written together
    /// in the same row-creation transaction so they are never
    /// independently NULL.
    pub theme_bg: String,
    /// #306 — child exit code captured by the daemon at `child.wait()`.
    /// `Some(_)` means the child returned via `exit()` / main return;
    /// `None` means either the child hasn't exited yet, was killed by a
    /// signal (see `signal_killed`), or the daemon died without writing
    /// the sidecar (DaemonLost; not surfaced in v1). Required column
    /// (NULL-able in SQL, but always serialized) per the [Required over
    /// Option] policy: the absence of an exit code is itself information
    /// the frontend renders, so a missing-field response is a bug.
    /// `required = true` flips the utoipa default ("Option ⇒ optional")
    /// so the OpenAPI schema marks the field as required-but-nullable,
    /// which `openapi-typescript` renders as `number | null` (no `?:`)
    /// — matching the contract intent: every response carries the
    /// field, even if its value is `null`.
    #[schema(value_type = Option<i32>, nullable = true, required = true)]
    pub exit_code: Option<i32>,
    /// #306 — true when the child was killed by a signal (SIGTERM,
    /// SIGKILL, SIGSEGV, …). Mutually exclusive with `exit_code.is_some()`
    /// at the writer: the daemon picks one branch on the way out and
    /// never both. Required (NOT NULL DEFAULT 0 in SQL) — every row
    /// carries a value, even if `false`.
    pub signal_killed: bool,
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
    /// #177 — host browser's theme RGB, threaded into the row-creation
    /// transaction. Required so the `terminals.theme_fg/_bg` NOT NULL
    /// columns always get a value at the same instant the row mints,
    /// closing the WS auto-revive race (see `ws::terminal::
    /// resolve_live_renderer` for the read side).
    pub theme: RequestTheme,
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
#[derive(Clone, Debug, ToSchema)]
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

// ---------------- Tasks (issue #644) ----------------

/// Worker kind a planned task lowers to at dispatch time.
///
/// Persisted as a lowercase string in `tasks.kind` (migration 0041).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Codex,
    Claude,
    Terminal,
}

/// Task plan status machine (design §3, issue #644). PR-A only ever
/// writes `pending` / `canceled` (the plan is inert); the scheduler
/// (PR-B) and gate runner (PR-C) drive the remaining transitions.
///
/// Persisted as a lowercase string in `tasks.status` (migration 0041).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Dispatched,
    Running,
    Verifying,
    Done,
    Failed,
    Canceled,
}

impl TaskStatus {
    /// Terminal statuses never transition again (a `canceled`/`failed`
    /// task is replaced by a new key, never revived — design §3.1).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled
        )
    }
}

/// One row of the track-scoped task plan (`tasks`, migration 0041).
///
/// `id = "{track_id}:{key}"` — kernel-composed (track ids are `new_id()`
/// hex, so `:` cannot collide). The JSON columns stay `String`s here:
/// the repo layer is mechanical and the tool layer owns
/// parse/normalize (`mcp_server::tools::plan`). Not exposed over REST
/// or the WS event stream in PR-A, hence no `ToSchema`/`TS` derives.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow, ToSchema)]
pub struct Task {
    pub id: String,
    pub track_id: String,
    pub key: String,
    pub kind: TaskKind,
    pub goal: String,
    pub context_json: String,
    pub acceptance_criteria: Option<String>,
    pub cwd: Option<String>,
    pub depends_on_json: String,
    pub priority: i64,
    pub gate_json: Option<String>,
    pub status: TaskStatus,
    pub status_detail: Option<String>,
    pub worker_card_id: Option<String>,
    pub gate_result_json: Option<String>,
    pub gate_attempt: i64,
    pub gate_pid: Option<i64>,
    pub gate_pid_starttime: Option<i64>,
    pub gate_pid_boot_id: Option<String>,
    pub running_deadline_ms: Option<i64>,
    pub context_stale_at_ms: Option<i64>,
    pub declared_by: String,
    /// Claim-frozen route selector. Deliberately not exposed through task
    /// read-state DTOs: it is written before claim, unlike child_track_id.
    pub spawn: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

impl Task {
    /// Parse `depends_on_json` back into sibling keys. The writer
    /// (`calm.plan.upsert`) always stores a sorted, deduped JSON array
    /// of strings, so a parse failure means out-of-band tampering —
    /// surface as empty rather than panicking (the column CHECK
    /// guarantees valid JSON, not shape).
    pub fn depends_on(&self) -> Vec<String> {
        serde_json::from_str(&self.depends_on_json).unwrap_or_default()
    }
}

// ---------------- Composites ----------------

/// What a Track detail page renders: the track itself plus its cards and
/// any overlays scoped to the track (status/progress badges) and its cards.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TrackDetail {
    pub track: Track,
    pub cards: Vec<Card>,
    pub overlays: Vec<Overlay>,
}

// ---------------- Helpers ----------------

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Deserializes `null` → `Some(None)`, missing → `None`, value → `Some(Some(v))`.
/// Used so `TrackPatch.archived_at` can distinguish "leave alone" from "set to null".
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
