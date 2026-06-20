//! Entity types — the core kernel vocabulary.
//!
//! #679 PR1: the IO-free entity/DTO vocabulary moved to `calm-types`
//! (`calm_types::model`) and is re-exported below, so every existing
//! `crate::model::Cove` / `calm_server::model::Card` path keeps working.
//! What stays defined here:
//!
//!   * route-coupled request DTOs (`NewWave` / `NewTerminal` carry a
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

pub use crate::ids::{ActorId, CardId, CoveId, WaveId};
// #679 PR1 — moved vocabulary, re-exported at the old paths. The source
// definitions live in calm-types; do NOT re-declare them here (shim-window
// type-drift risk, issue #679 "Greenfield-specific risks" #4).
pub use calm_types::model::{
    Card, CardRole, CardRuntimeView, Cove, CoveFolder, CoveKind, CoveResolve, FolderConflict,
    FolderConflictKind, HarnessItem, Overlay, Wave, WaveLifecycle, default_deletable,
};

/// Wire shape of `NewCodexCardBody.theme` / `NewWave.theme`. Matches the
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

// ---------------- Cove DTOs ----------------

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

// ---------------- CoveFolder DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCoveFolder {
    /// Absolute filesystem path. Must start with `/`. The server trims
    /// a trailing slash before insert (root `/` excepted) so equality
    /// and prefix matching stay canonical.
    pub path: String,
}

// ---------------- Wave DTOs ----------------

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
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
    /// Host browser's current theme RGB (#177). Required end-to-end so
    /// the auto-minted spec card's terminal renderer answers codex's
    /// OSC 10/11 startup probe with matching colors. A body
    /// missing this field is rejected at the deserialize layer (422):
    /// the spec card is invisible to the user and a silent fallback
    /// would mean every wave-from-the-UI spawned with a mis-tinted
    /// composer (the bug that motivated this refactor).
    ///
    /// Direct repo callers (`db::sqlite::wave_create_tx`, used by tests
    /// and a couple of non-route helpers) still pass a value here even
    /// though the txn-level helper does not consume it — spec-card
    /// spawning is owned by `routes::waves::create_wave`. Tests can
    /// use `RequestTheme::default_dark()` as a no-op sentinel.
    pub theme: RequestTheme,
}

#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
pub struct WavePatch {
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
    /// transition validation runs through `crate::wave_lifecycle`,
    /// inside the write transaction. Omitting (`None`) means "leave
    /// alone"; `Some(<state>)` triggers the validator against the
    /// (actor, from → to) triple before any DB write or event emit.
    pub lifecycle: Option<WaveLifecycle>,
    /// Issue #644 — per-wave scheduler budget (`waves.task_budget`,
    /// migration 0041). Pass `Some(Some(n))` to set, `Some(None)` to
    /// clear back to the kernel default, or omit (`None`) to leave
    /// alone. Inert until the PR-B scheduler reads it.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub task_budget: Option<Option<i64>>,
    /// Issue #644 — wave-level gate policy (`waves.require_task_gates`,
    /// migration 0041). `Some(v)` sets the flag, omit to leave alone.
    /// Enforced by `calm.plan.upsert` rule 6 only from PR-C onward.
    pub require_task_gates: Option<bool>,
}

// ---------------- Card DTOs ----------------

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
/// `claude` is deliberately absent: no claude-worker dispatch adapter
/// exists, the column CHECK omits it, and `calm.plan.upsert` rejects it
/// with an explicit "not yet supported" error so a later migration can
/// add the variant together with the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Codex,
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

/// One row of the wave-scoped task plan (`tasks`, migration 0041).
///
/// `id = "{wave_id}:{key}"` — kernel-composed (wave ids are `new_id()`
/// hex, so `:` cannot collide). The JSON columns stay `String`s here:
/// the repo layer is mechanical and the tool layer owns
/// parse/normalize (`mcp_server::tools::plan`). Not exposed over REST
/// or the WS event stream in PR-A, hence no `ToSchema`/`TS` derives.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow, ToSchema)]
pub struct Task {
    pub id: String,
    pub wave_id: String,
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
