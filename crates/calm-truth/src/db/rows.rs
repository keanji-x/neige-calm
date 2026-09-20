//! sqlx row wrappers for the calm-types entities: calm-types is sqlx-free and the orphan rule forbids `FromRow` on
//! its types, so each entity gets a thin `XRow` mirror converted via `From<XRow> for X`. Field lists mirror the SELECT column lists 1:1.

use crate::ids::{AreaId, CardId, TrackId};
use crate::model::{
    Area, AreaFolder, AreaKind, Card, HarnessInputSegment, HarnessItem, Overlay, Track,
    TrackLifecycle, TrackWorkspace, TrackWorkspaceKind,
};
use calm_types::claude_permissions::ClaudePermissionsScope;

/// Row mirror of [`Area`].
#[derive(Debug, sqlx::FromRow)]
pub struct AreaRow {
    #[sqlx(try_from = "String")]
    pub id: AreaId,
    pub name: String,
    pub color: String,
    pub sort: f64,
    #[sqlx(try_from = "String")]
    pub kind: AreaKind,
    pub default_template_id: Option<String>,
    pub default_cwd: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<AreaRow> for Area {
    fn from(r: AreaRow) -> Self {
        Area {
            id: r.id,
            name: r.name,
            color: r.color,
            sort: r.sort,
            kind: r.kind,
            default_template_id: r.default_template_id,
            default_cwd: r.default_cwd,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Row mirror of [`AreaFolder`].
#[derive(Debug, sqlx::FromRow)]
pub struct AreaFolderRow {
    pub id: i64,
    #[sqlx(try_from = "String")]
    pub area_id: AreaId,
    pub path: String,
    pub created_at: i64,
}

impl From<AreaFolderRow> for AreaFolder {
    fn from(r: AreaFolderRow) -> Self {
        AreaFolder {
            id: r.id,
            area_id: r.area_id,
            path: r.path,
            created_at: r.created_at,
        }
    }
}

/// The `tracks` column list every `query_as::<_, TrackRow>` SELECT must use, in `TrackRow` field order. `query_as`
/// binds columns by name at **runtime**, so a stale hand-copied list compiles fine and blows up in production.
pub const TRACK_SELECT_COLUMNS: &str = "id, area_id, title, sort, archived_at, pinned_at, lifecycle, template_id, \
     plugin_scope, purpose, template_input, terminal_at, recipe_id, recipe_revision, \
     workspace_kind, workspace_path, workspace_frozen_at, created_at, updated_at, \
     claude_permissions_policy";

/// [`TRACK_SELECT_COLUMNS`] with every column qualified by the `w` table alias; the two lists must stay in lockstep.
pub const TRACK_SELECT_COLUMNS_W: &str = "w.id, w.area_id, w.title, w.sort, w.archived_at, w.pinned_at, w.lifecycle, \
     w.template_id, w.plugin_scope, w.purpose, w.template_input, w.terminal_at, \
     w.recipe_id, w.recipe_revision, w.workspace_kind, w.workspace_path, \
     w.workspace_frozen_at, w.created_at, w.updated_at, w.claude_permissions_policy";

/// Row mirror of [`Track`].
#[derive(Debug, sqlx::FromRow)]
pub struct TrackRow {
    #[sqlx(try_from = "String")]
    pub id: TrackId,
    #[sqlx(try_from = "String")]
    pub area_id: AreaId,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub pinned_at: Option<i64>,
    #[sqlx(try_from = "String")]
    pub lifecycle: TrackLifecycle,
    pub template_id: Option<String>,
    pub plugin_scope: Option<String>,
    pub purpose: Option<String>,
    /// Nullable JSON TEXT column; `nullable` so a NULL lands as `None` instead of a decode error.
    #[sqlx(json(nullable))]
    pub template_input: Option<serde_json::Value>,
    pub terminal_at: Option<i64>,
    /// The user recipe this track was instantiated from and its revision at that moment; both NULL for every other
    /// creation source. `recipe_id` is a record of origin, not a live reference — the recipe may since be edited or deleted.
    pub recipe_id: Option<String>,
    pub recipe_revision: Option<i64>,
    /// The three columns behind [`TrackWorkspace`]; `workspace_path` is the only stored copy of the path.
    #[sqlx(try_from = "String")]
    pub workspace_kind: TrackWorkspaceKind,
    pub workspace_path: String,
    pub workspace_frozen_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// The tree root's Claude Code permission policy as stored (a child row is NULL); a value the scope derive cannot
    /// decode fails the read (an error, never "no policy").
    #[sqlx(json(nullable))]
    pub claude_permissions_policy: Option<ClaudePermissionsScope>,
}

impl From<TrackRow> for Track {
    fn from(r: TrackRow) -> Self {
        Track {
            id: r.id,
            area_id: r.area_id,
            title: r.title,
            sort: r.sort,
            archived_at: r.archived_at,
            pinned_at: r.pinned_at,
            lifecycle: r.lifecycle,
            // The one place the wire alias is computed.
            cwd_wire_alias: r.workspace_path.clone(),
            template_id: r.template_id,
            plugin_scope: r.plugin_scope,
            purpose: r.purpose,
            template_input: r.template_input,
            terminal_at: r.terminal_at,
            recipe_id: r.recipe_id,
            recipe_revision: r.recipe_revision,
            workspace: TrackWorkspace {
                kind: r.workspace_kind,
                path: r.workspace_path,
                frozen_at: r.workspace_frozen_at,
            },
            claude_permissions_policy: r.claude_permissions_policy,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[cfg(test)]
mod track_select_columns_tests {
    use super::{TRACK_SELECT_COLUMNS, TRACK_SELECT_COLUMNS_W};

    /// The aliased list must be the unaliased list with `w.` in front of each name, same order. This only defends the two
    /// constants against each other — a field added to `TrackRow` and forgotten in *both* lists stays green here and fails
    /// at runtime; only a test that executes a SELECT and reads the new column back catches that.
    #[test]
    fn track_select_columns_lists_agree() {
        let plain: Vec<String> = TRACK_SELECT_COLUMNS
            .split(',')
            .map(|c| c.trim().to_string())
            .collect();
        let aliased: Vec<String> = TRACK_SELECT_COLUMNS_W
            .split(',')
            .map(|c| c.trim().to_string())
            .collect();
        let expected: Vec<String> = plain.iter().map(|c| format!("w.{c}")).collect();
        assert_eq!(aliased, expected);
    }
}

/// Row mirror of [`Card`]. `Card.runtime` is a lazy projection joined after the fetch, never a `cards` column; the conversion seeds it `None`.
#[derive(Debug, sqlx::FromRow)]
pub struct CardRow {
    #[sqlx(try_from = "String")]
    pub id: CardId,
    #[sqlx(try_from = "String")]
    pub track_id: TrackId,
    pub kind: String,
    pub sort: f64,
    #[sqlx(json)]
    pub payload: serde_json::Value,
    pub title: Option<String>,
    pub deletable: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<CardRow> for Card {
    fn from(r: CardRow) -> Self {
        Card {
            id: r.id,
            track_id: r.track_id,
            kind: r.kind,
            sort: r.sort,
            payload: r.payload,
            title: r.title,
            runtime: None,
            deletable: r.deletable,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// SQL row mirror of the public transcript record.
#[derive(Debug, sqlx::FromRow)]
pub struct HarnessItemRow {
    pub id: i64,
    pub worker_session_id: String,
    #[sqlx(try_from = "String")]
    pub card_id: CardId,
    #[sqlx(try_from = "String")]
    pub track_id: TrackId,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub item_uuid: Option<String>,
    pub item_type: Option<String>,
    pub method: String,
    pub params: String,
    pub input_segments: Option<String>,
    pub created_at_ms: i64,
}

impl TryFrom<HarnessItemRow> for HarnessItem {
    type Error = String;

    fn try_from(r: HarnessItemRow) -> Result<Self, Self::Error> {
        let input_segments = r
            .input_segments
            .map(|value| {
                serde_json::from_str::<Vec<HarnessInputSegment>>(&value)
                    .map_err(|error| error.to_string())
            })
            .transpose()?;
        Ok(HarnessItem {
            id: r.id,
            worker_session_id: r.worker_session_id,
            card_id: r.card_id,
            track_id: r.track_id,
            thread_id: r.thread_id,
            turn_id: r.turn_id,
            item_uuid: r.item_uuid,
            item_type: r.item_type,
            method: r.method,
            params: r.params,
            input_segments,
            created_at_ms: r.created_at_ms,
        })
    }
}

/// Row of the `worker_flow_items` table: the raw persistence shape, not a mirror of a calm-types entity.
/// `card_id` and `worker_session_id` are `ON DELETE SET NULL` FKs, so a row survives its card/session; `captured_session_id`
/// has no FK and outlives the session. Plain `String` ids keep the decode total for orphaned rows.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct WorkerFlowItemRow {
    pub id: i64,
    pub card_id: Option<String>,
    pub captured_session_id: Option<String>,
    pub track_id: Option<String>,
    pub worker_session_id: Option<String>,
    pub kind: String,
    pub payload: String,
    pub created_at_ms: i64,
}

/// Per-card source cursor for passive worker-flow capture.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct WorkerFlowCursor {
    pub card_id: String,
    pub source_kind: String,
    pub source_path: String,
    pub record_index: i64,
    pub byte_offset: i64,
    pub last_source_uuid: Option<String>,
    pub last_line_hash: Option<String>,
    pub updated_at_ms: i64,
}

/// Row mirror of [`Overlay`].
#[derive(Debug, sqlx::FromRow)]
pub struct OverlayRow {
    pub id: String,
    pub plugin_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub kind: String,
    #[sqlx(json)]
    pub payload: serde_json::Value,
    pub updated_at: i64,
}

impl From<OverlayRow> for Overlay {
    fn from(r: OverlayRow) -> Self {
        Overlay {
            id: r.id,
            plugin_id: r.plugin_id,
            entity_kind: r.entity_kind,
            entity_id: r.entity_id,
            kind: r.kind,
            payload: r.payload,
            updated_at: r.updated_at,
        }
    }
}
