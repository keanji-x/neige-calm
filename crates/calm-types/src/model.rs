//! Entity types — the core kernel vocabulary. Everything else lives in plugins and reaches the
//! kernel as opaque JSON in `Card.payload` or `Overlay.payload`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::claude_permissions::ClaudePermissionsScope;
pub use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::planner_attachment::PlannerAttachment;
use crate::runtime::{AgentProvider, WorkerSessionKind};
use crate::worker::WorkerSessionState;

/// Authorization role persisted on each card and enforced by `role_gate`.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum CardRole {
    #[default]
    Worker,
    Planner,
    ReportCard,
    /// A track-scoped assistant conversation with no lifecycle / plan / review / admin authority.
    Assistant,
}

impl CardRole {
    pub fn as_db_str(self) -> &'static str {
        match self {
            CardRole::Worker => "worker",
            CardRole::Planner => "planner",
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
            "planner" => Ok(CardRole::Planner),
            "reportcard" => Ok(CardRole::ReportCard),
            "assistant" => Ok(CardRole::Assistant),
            other => Err(format!("unknown cards.role value `{other}`")),
        }
    }
}

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
    /// Built-in Track template preselected by the official New Track surface; `None` keeps "No template".
    #[serde(default)]
    #[schema(nullable = true, required = true)]
    pub default_template_id: Option<String>,
    /// Exact attached Git working directory preselected for a new Track; `None` keeps the server-managed default.
    #[serde(default)]
    #[schema(nullable = true, required = true)]
    pub default_cwd: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row per claimed directory; `path` is absolute and globally unique across the table. A folder
/// covers every descendant path, and overlapping claims are rejected.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct AreaFolder {
    pub id: i64,
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub path: String,
    pub created_at: i64,
}

/// Kind of overlap detected by the `POST /api/areas/:area_id/folders` conflict check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum FolderConflictKind {
    /// Proposed path equals an existing folder's path exactly.
    Equal,
    /// Proposed path is an ancestor of an existing folder.
    Ancestor,
    /// Proposed path is a descendant of an existing folder.
    Descendant,
}

/// 409 body for the folder-create conflict case.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct FolderConflict {
    pub folder_id: i64,
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub conflict_path: String,
    pub conflict_kind: FolderConflictKind,
}

/// 200 body for `GET /api/areas/resolve`; the endpoint returns `null` (not 404) on miss.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct AreaResolve {
    #[schema(value_type = String)]
    pub area_id: AreaId,
    pub folder_id: i64,
    pub folder_path: String,
}

/// Track lifecycle state machine. `archived` is intentionally NOT a lifecycle state: archival lives
/// on `archived_at`, orthogonal to execution semantics.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS,
)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum TrackLifecycle {
    /// New track; user is editing goal/context and hasn't handed off to the Planner Agent yet.
    #[default]
    Draft,
    /// Planner Agent is reading the goal + code context and producing a plan.
    Planning,
    /// Planner Agent has emitted one or more dispatch requests and the
    /// Dispatcher is spawning worker cards.
    Dispatching,
    /// At least one worker card is executing; the track has not reached
    /// review.
    Working,
    /// Track needs human input, or a worker failed in a way the Planner
    /// Agent cannot recover from autonomously.
    Blocked,
    /// Workers have produced results; Planner Agent or the user is
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
    /// Is this a terminal state? Terminal states cannot transition except via a user-driven reopen.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TrackLifecycle::Done | TrackLifecycle::Canceled | TrackLifecycle::Failed
        )
    }

    /// The lowercase string persisted in `tracks.lifecycle`.
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
    /// One-shot, monotonic. `Some` ⇒ neither `path` nor `kind` may change again. The system-area
    /// launchpad stays unfrozen because it is repointed.
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

/// Purpose marker on retired Area-conversation tracks.
pub const AREA_CHAT_PURPOSE: &str = "area-chat";

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
    /// Wire-compatibility alias of `workspace.path`, serialized as `cwd`; Rust readers must use `workspace.path`.
    #[serde(rename = "cwd", default)]
    pub cwd_wire_alias: String,
    /// Template this track was created from. Names the birth snapshot only.
    // The alias is a deserialization-only read for pre-rename event-log rows; keep BOTH `default` and
    // `alias`, or old rows replay as `None` / get skipped. Non-doc on purpose: doc comments here are
    // exported into generated artifacts.
    #[serde(default, alias = "workflow_id")]
    pub template_id: Option<String>,
    /// The plugin that owns this track, recorded when the row is created.
    #[serde(default)]
    pub plugin_scope: Option<String>,
    /// Server-owned structural marker. Public track creation cannot set this.
    #[serde(default)]
    pub purpose: Option<String>,
    /// Template input is validated at creation and otherwise remains opaque.
    #[serde(default, alias = "workflow_input")]
    #[schema(value_type = Option<Object>)]
    #[ts(type = "unknown")]
    pub template_input: Option<serde_json::Value>,
    /// Unix-ms timestamp the track most recently entered a terminal lifecycle state, or `None` while non-terminal.
    #[serde(default)]
    pub terminal_at: Option<i64>,
    /// The user recipe ([`TrackRecipe`]) this track was instantiated from; may name a recipe that no longer exists.
    #[serde(default)]
    pub recipe_id: Option<String>,
    /// The recipe's `revision` at the moment this track was created.
    #[serde(default)]
    pub recipe_revision: Option<i64>,
    #[serde(default)]
    pub workspace: TrackWorkspace,
    /// The user-set Claude Code permission policy of this track's TREE, stored on the tree root only;
    /// a child row is always `null` here even when its root carries a policy.
    #[serde(default)]
    pub claude_permissions_policy: Option<ClaudePermissionsScope>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Live runtime projection read from `worker_sessions` when a card is fetched or serialized. Not
/// part of the idempotency contract: it may differ between a first POST response and a retry.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct CardRuntimeView {
    pub worker_session_id: String,
    pub kind: WorkerSessionKind,
    pub status: WorkerSessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub updated_at_ms: Option<i64>,
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
    /// When the card's last non-interrupted turn ended, or absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_turn_completed_ms: Option<i64>,
}

#[cfg(test)]
mod runtime_view_tests;

/// One row of `GET /api/tracks/{track_id}/conversations`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackConversationSummary {
    /// The assistant card's id; the conversation's identity everywhere.
    pub id: String,
    /// The track this conversation lives on.
    pub track_id: String,
    /// The conversation's own name, or null before it has one.
    pub title: Option<String>,
    /// Always `"track-assistant"`, derived from the card's persisted marker.
    pub kind: String,
    /// The live session's state, or **null when the card has no session row**. Never fill it with an invented value.
    pub state: Option<WorkerSessionState>,
    /// The session's last update, falling back to the card's own.
    pub updated_at: i64,
    /// When the conversation's last non-interrupted turn ended, or `null` when there is none.
    pub last_turn_completed_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Card {
    #[schema(value_type = String)]
    pub id: CardId,
    #[schema(value_type = String)]
    pub track_id: TrackId,
    /// `"terminal"` for built-in PTY cards, `"ui://<plugin>/<view>"` for plugin-provided cards.
    /// `[legacy]` `"plugin:<plugin-id>:<view-id>"` may still appear on persisted rows.
    pub kind: String,
    pub sort: f64,
    #[schema(value_type = Object)]
    /// Opaque JSON blob.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<CardRuntimeView>,
    /// System-card guard: `true` for user-facing cards, `false` for kernel-owned cards the user cannot
    /// remove. Defaults to `true` because `bool::default()` is the unsafe fallback for a deny-by-omission auth bit.
    #[serde(default = "default_deletable")]
    pub deletable: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Default for `Card.deletable` when wire payloads / replay fixtures omit the field.
pub fn default_deletable() -> bool {
    true
}

/// How one segment of a harness `userMessage` should be presented to a human. The rendered English
/// is not a protocol: wording changes must not turn a system update into something the UI attributes to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum HarnessInputPresentation {
    User,
    System,
    SystemWorkerTurnFinished,
    SystemReportEdited,
    SystemTaskCompleted,
    SystemTaskFailed,
}

/// One observation in the exact order and wording sent to `turn/start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessInputSegment {
    pub presentation: HarnessInputPresentation,
    pub text: String,
    /// The images this segment carried into `turn/start`.
    #[serde(default)]
    pub attachments: Vec<PlannerAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessItem {
    pub id: i64,
    pub worker_session_id: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_segments: Option<Vec<HarnessInputSegment>>,
    pub created_at_ms: i64,
}

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
    /// Opaque JSON blob.
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    pub updated_at: i64,
}

#[cfg(test)]
mod card_role_tests {
    use super::CardRole;

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        for (role, json) in [
            (CardRole::Worker, "\"worker\""),
            (CardRole::Planner, "\"planner\""),
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
        for role in [CardRole::Worker, CardRole::Planner, CardRole::ReportCard] {
            let wire = serde_json::to_string(&role).expect("serialize");
            assert_eq!(format!("\"{}\"", role.as_db_str()), wire);
            let back = CardRole::try_from(role.as_db_str().to_string()).expect("decode");
            assert_eq!(back, role);
        }
        assert!(CardRole::try_from("bogus".to_string()).is_err());
    }
}

/// A user-defined starting point for a new track: a saved report whose `title` doubles as the
/// summary and whose `neige-block` fences are its tasks.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackRecipe {
    pub id: String,
    /// Picker label *and* the instantiated report's summary.
    pub title: String,
    /// Report body. Its `neige-block` fences are the tasks.
    pub body: String,
    /// Optimistic-lock anchor. Writers pass the revision they read and the UPDATE validates + bumps in one statement.
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
        for state in ALL {
            let wire = serde_json::to_string(&state).expect("serialize");
            assert_eq!(format!("\"{}\"", state.as_db_str()), wire);
            let back = TrackLifecycle::try_from(state.as_db_str().to_string()).expect("decode");
            assert_eq!(back, state);
        }
        assert!(TrackLifecycle::try_from("bogus".to_string()).is_err());
    }
}
