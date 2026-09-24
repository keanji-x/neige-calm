//! Entity types — the core kernel vocabulary. IO-free vocabulary lives in
//! `calm-types` and is re-exported; route-coupled DTOs and sqlx-coupled
//! entities stay here. Patch structs use `Option<T>`: `None` = leave alone.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use crate::ids::{ActorId, AreaId, CardId, TrackId};
use calm_types::claude_permissions::{ClaudePermissionsScope, parse_scope_named};
// Source definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::model::{
    Area, AreaFolder, AreaKind, AreaResolve, Card, CardRole, CardRuntimeView, FolderConflict,
    FolderConflictKind, HarnessInputPresentation, HarnessInputSegment, HarnessItem, NewTrackRecipe,
    Overlay, Track, TrackConversationSummary, TrackLifecycle, TrackRecipe, TrackWorkspace,
    TrackWorkspaceKind, default_deletable,
};

/// Wire shape of `NewCodexCardBody.theme` / `NewTrack.theme`; duplicates
/// `calm_session::TerminalTheme` so the route keeps its own `ToSchema`.
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

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewAreaFolder {
    /// Absolute filesystem path; the server trims a trailing slash (root `/` excepted).
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTrack {
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub title: String,
    pub sort: Option<f64>,
    /// Absolute path the planner daemon spawns under; the route validates,
    /// `track_create_tx` writes verbatim.
    pub cwd: String,
    #[serde(default)]
    pub template_id: Option<String>,
    /// Copied from the owning Manifest at create; not accepted on `POST /api/tracks`.
    #[serde(default)]
    pub plugin_scope: Option<String>,
    /// JSON input for the bound template: validated against the Manifest's
    /// `input_schema` by the route, persisted verbatim, never interpreted by the kernel.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub template_input: Option<serde_json::Value>,
    /// Claim `cwd` for `area_id` as a new folder in the same transaction as the
    /// track create. Default `false`: the cwd must already be covered by a folder.
    #[serde(default)]
    pub attach_folder: bool,
    /// Host browser's theme RGB, required so the planner card's terminal answers
    /// codex's OSC 10/11 probe with matching colors. Tests use `default_dark()`.
    pub theme: RequestTheme,
}

/// `plugin_scope` is create-time only and is not a field here. Extra JSON keys are ignored.
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
    /// Request a lifecycle transition; validated through `crate::track_lifecycle`
    /// inside the write transaction.
    pub lifecycle: Option<TrackLifecycle>,
    /// Per-track scheduler budget; `Some(None)` clears back to the kernel default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub task_budget: Option<Option<i64>>,
    /// Maximum admitted planner-declared task inventory. A present null resets to
    /// the kernel default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub planner_task_ceiling: Option<Option<i64>>,
    /// Per-track declaration policy. A present null resets to the kernel default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub automation_policy: Option<Option<String>>,
    /// Budget for the non-terminal planner inventory of the WHOLE track tree.
    /// Root-only; a present null resets to the kernel default (32).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub tree_task_budget: Option<Option<i64>>,
    /// Claude Code permission policy of the WHOLE track tree; tree-root-only.
    /// `Some(None)` clears. Read through `parse_scope_named`, so a bad shape is an
    /// error naming the field, never a lenient decode.
    #[serde(default, deserialize_with = "deserialize_double_option_policy")]
    #[schema(value_type = Option<ClaudePermissionsScope>)]
    pub claude_permissions_policy: Option<Option<ClaudePermissionsScope>>,
    /// Track-level gate policy; `Some(v)` sets the flag.
    pub require_task_gates: Option<bool>,
    /// Request a workspace change. Handled by the route, never by
    /// `track_update_tx`: a re-point is a filesystem move bracketed by two
    /// transactions, and mutually exclusive with every other field here.
    #[serde(default)]
    pub workspace: Option<TrackWorkspacePatch>,
}

/// Point a track at a repository the user already has. The only transition is
/// `managed → attached`: a managed path is derived from area and id, and
/// `attached → *` stays refused, so this is a one-way door stamping `frozen_at`.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackWorkspacePatch {
    /// Must be `attached`. `managed` is a documented 400, not a silent no-op.
    pub kind: TrackWorkspaceKind,
    /// Absolute path to an existing Git work tree, validated before anything is written.
    pub path: String,
    /// Claim `path` for this track's area in the same transaction. Default
    /// `false`: an unclaimed path is refused rather than making a homeless track.
    #[serde(default)]
    pub attach_folder: bool,
}

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewCard {
    /// Defaulted so the REST handler can override from the `:track_id` path
    /// param; direct repo callers must set it.
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
    /// Replaces the stored payload. A payload carrying a server-owned key is refused with 400:
    /// `terminal_signals`, `claude_permissions`, `claude_permissions_source`, `template_context`,
    /// `planner_provider`. Every server-owned key the card already carries is kept.
    #[schema(value_type = Option<Object>)]
    pub payload: Option<serde_json::Value>,
    /// Not patchable via API: surfaced only so a client sending it gets a clear
    /// 400; `card_update_tx` never writes the column.
    pub deletable: Option<bool>,
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
    /// Child process id; `None` for rows whose spawn returned no pid.
    pub pid: Option<i64>,
    /// Host browser's foreground RGB at row-creation, comma-decimal `r,g,b`.
    pub theme_fg: String,
    /// Host browser's background RGB at row-creation; written together with `theme_fg`.
    pub theme_bg: String,
    /// Exit code captured at `child.wait()`; `None` = not exited, signal-killed,
    /// or daemon lost. `required = true` makes the OpenAPI field
    /// required-but-nullable so clients get `number | null`, not `?:`.
    #[schema(value_type = Option<i32>, nullable = true, required = true)]
    pub exit_code: Option<i32>,
    /// True when the child was killed by a signal; mutually exclusive with
    /// `exit_code.is_some()` at the writer.
    pub signal_killed: bool,
    /// Bounded tail of the merged PTY byte stream, decoded lossily as UTF-8.
    pub pty_output: String,
    /// True when bytes were dropped before `pty_output`, either by the
    /// supervisor replay window or the smaller durable-evidence cap.
    pub pty_output_truncated: bool,
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
    /// Host browser's theme RGB; required so the NOT NULL theme columns get a
    /// value at the instant the row mints.
    pub theme: RequestTheme,
}

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

/// `version` is read off the manifest and stored as a denormalized index column.
#[derive(Clone, Debug, ToSchema)]
pub struct NewPlugin {
    pub id: String,
    pub version: String,
    pub install_path: String,
    pub manifest: serde_json::Value,
    /// Plugins land disabled by default; `true` here is an explicit choice.
    pub enabled: bool,
    pub user_config: serde_json::Value,
}

/// Worker kind a planned task lowers to at dispatch time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Codex,
    Claude,
    Terminal,
}

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
    /// Terminal executions never transition again; failed-work recovery allocates
    /// a new execution ID under the same Track + key.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled
        )
    }
}

/// One row of the track-scoped task plan. `id` names an execution: first
/// executions use `"{track_id}:{key}"`, recovered ones have distinct IDs.
/// JSON columns stay `String`s; the tool layer owns parse/normalize.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow, ToSchema)]
pub struct Task {
    pub id: String,
    pub track_id: String,
    pub key: String,
    pub kind: TaskKind,
    /// Stored in `tasks.goal`; public surfaces call it `goal` for agent kinds and
    /// `command` for terminal.
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
    /// The writer always stores a sorted, deduped JSON array, so a parse failure
    /// means tampering — surface as empty rather than panicking.
    pub fn depends_on(&self) -> Vec<String> {
        serde_json::from_str(&self.depends_on_json).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TrackDetail {
    pub track: Track,
    /// Lifecycle permission and child-track integrity resolved together so
    /// clients never advertise an action the write must reject.
    pub can_resume: bool,
    pub cards: Vec<Card>,
    pub overlays: Vec<Overlay>,
}

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

/// `null` → `Some(None)`, missing → `None`, otherwise the strict
/// [`parse_scope_named`] shape, whose reason becomes the deserialization error.
fn deserialize_double_option_policy<'de, D>(
    d: D,
) -> Result<Option<Option<ClaudePermissionsScope>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Deserialize::deserialize(d)?;
    match value {
        None => Ok(Some(None)),
        Some(value) => parse_scope_named("claude_permissions_policy", &value)
            .map(|scope| Some(Some(scope)))
            .map_err(serde::de::Error::custom),
    }
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
