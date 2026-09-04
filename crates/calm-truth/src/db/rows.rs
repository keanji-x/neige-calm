//! sqlx row wrappers for the calm-types entities (#679 PR1).
//!
//! calm-types is sqlx-free by design (zero-IO compile firewall), and the
//! orphan rule forbids implementing `sqlx::FromRow` for its types from
//! here. So every moved entity gets a thin `XRow` mirror that derives
//! `FromRow` and converts via `From<XRow> for X`:
//!
//! ```text
//!   query_as::<_, TrackRow>(…).fetch_one(…).await?.into()   // → Track
//! ```
//!
//! Field lists mirror the SELECT column lists 1:1; typed ids and persisted
//! enums decode through `#[sqlx(try_from = "String")]` against the
//! `TryFrom<String>` impls in calm-types (ids are infallible via their
//! `From<String>`; enums reject unknown strings, same behavior as the old
//! `sqlx::Type` derive). Binds use `.as_str()` / `.as_db_str()` at the call
//! sites — the stored TEXT shapes are unchanged and pinned by calm-types
//! tests.
//!
//! This module is the shim-window home for row mapping; #679 PR2 moves it
//! into calm-truth together with the repos.

use crate::ids::{AreaId, CardId, TrackId};
use crate::model::{
    Area, AreaFolder, AreaKind, Card, HarnessInputSegment, HarnessItem, Overlay, Track,
    TrackLifecycle, TrackWorkspace, TrackWorkspaceKind,
};

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

/// The `tracks` column list every `query_as::<_, TrackRow>` SELECT must use,
/// in `TrackRow` field order.
///
/// Issue #1147 S1: this used to be nine hand-copied literals. `query_as`
/// binds columns by name at **runtime**, so a column added to `TrackRow`
/// without touching all nine SELECTs compiles fine and then blows up in
/// production on whichever route happened to keep the stale list — the
/// `tracks` replay of the `CardRow` incident. One const kills the class.
/// Use [`TRACK_SELECT_COLUMNS_W`] where the query aliases the table as `w`.
pub const TRACK_SELECT_COLUMNS: &str = "id, area_id, title, sort, archived_at, pinned_at, lifecycle, template_id, \
     plugin_scope, purpose, template_input, terminal_at, recipe_id, recipe_revision, \
     workspace_kind, workspace_path, workspace_frozen_at, created_at, updated_at";

/// [`TRACK_SELECT_COLUMNS`] with every column qualified by the `w` table alias.
/// `#[sqlx(flatten)]` / `FromRow` still resolve the *unqualified* names, so the
/// two lists must stay in lockstep — `track_select_columns_lists_agree` pins that.
pub const TRACK_SELECT_COLUMNS_W: &str = "w.id, w.area_id, w.title, w.sort, w.archived_at, w.pinned_at, w.lifecycle, \
     w.template_id, w.plugin_scope, w.purpose, w.template_input, w.terminal_at, \
     w.recipe_id, w.recipe_revision, w.workspace_kind, w.workspace_path, \
     w.workspace_frozen_at, w.created_at, w.updated_at";

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
    /// Nullable JSON TEXT column (migration 0061); decodes through the same
    /// `#[sqlx(json)]` machinery as `CardRow.payload`, `nullable` so a NULL
    /// column lands as `None` instead of a decode error.
    #[sqlx(json(nullable))]
    pub template_input: Option<serde_json::Value>,
    pub terminal_at: Option<i64>,
    /// #1292 S3 — migration 0085. The user recipe this track was instantiated
    /// from, and the recipe revision that was current at that moment. Both
    /// NULL for every other creation source; a DB CHECK refuses one-without-
    /// the-other. `recipe_id` may name a recipe that has since been edited or
    /// deleted — the track holds a copy, so the id is a record of origin, not
    /// a live reference.
    pub recipe_id: Option<String>,
    pub recipe_revision: Option<i64>,
    /// #1147 S1 — migration 0077. The three columns behind [`TrackWorkspace`].
    /// `workspace_path` is the only stored copy of the path; the old `cwd`
    /// column was dropped by that same migration.
    #[sqlx(try_from = "String")]
    pub workspace_kind: TrackWorkspaceKind,
    pub workspace_path: String,
    pub workspace_frozen_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
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
            // The one place the wire alias is computed. There is no other
            // source for it: the column it used to mirror no longer exists.
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
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[cfg(test)]
mod track_select_columns_tests {
    use super::{TRACK_SELECT_COLUMNS, TRACK_SELECT_COLUMNS_W};

    /// The aliased list must be the unaliased list with `w.` in front of each
    /// name — nothing added, nothing dropped, same order. Without this the two
    /// consts drift and the aliased `track_detail` SELECT silently loses a
    /// column at runtime, which is the exact failure the consts exist to stop.
    ///
    /// # Read this before trusting a green run here
    ///
    /// **This test defends the consistency of the two constants with each
    /// other. It does not defend the consistency of either constant with
    /// [`TrackRow`] or with the `tracks` table.**
    ///
    /// It never looks at `TrackRow`'s fields and never looks at the schema. So
    /// the most natural mistake — adding a field to `TrackRow` and forgetting
    /// *both* lists — leaves this test green, and the SELECTs then fail at
    /// runtime with `no column found for name: …`, because `query_as` binds by
    /// name when the query runs, not when it compiles. What it does catch is any
    /// asymmetry between the two lists — it compares names *and* order after
    /// stripping the `w.` prefix, so a one-sided drop, a one-sided addition, a
    /// one-sided rename and a one-sided reorder all turn it red. What escapes it
    /// is both lists missing the same column.
    ///
    /// The thing that catches a symmetric omission is a test that actually
    /// executes a SELECT and reads the new column back. #1292 S3 added two,
    /// one per constant, in
    /// `calm-server/tests/cases/track_recipe_instantiate.rs`:
    /// `a_recipe_created_track_records_which_recipe_and_which_revision` covers
    /// [`TRACK_SELECT_COLUMNS`] and `the_track_detail_route_carries_the_provenance`
    /// covers [`TRACK_SELECT_COLUMNS_W`]. If you add a column here, add a read
    /// assertion there too — green here is not the same as safe.
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

/// Row mirror of [`Card`].
///
/// `Card.runtime` is `#[sqlx(skip)]` in spirit: it is a lazy projection
/// joined after the fetch (`session_projection_projectable_for_card`), never a
/// `cards` column — the conversion seeds it `None` exactly like the old
/// derive did.
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
    pub runtime_id: String,
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
            runtime_id: r.runtime_id,
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

/// Row of the `worker_flow_items` table (#695 PR2).
///
/// Sibling of the planner transcript row, but deliberately *not* a mirror of a
/// calm-types model entity: it is the raw persistence shape for the
/// worker message-flow capture table, returned straight to callers
/// (no `From<…>` projection — PR3's sink/projection owns that).
///
/// `card_id` is `Option<String>` because the table's FK is
/// `REFERENCES cards(id) ON DELETE SET NULL` — a row must survive the
/// deletion of its worker card (#695), so this column goes NULL rather
/// than cascading away.
///
/// `worker_session_id` and `captured_session_id` hold the same value on every
/// insert the PR5 sink makes, and are still NOT redundant — which is why
/// #1316 S4a renamed the second rather than deleting it. `worker_session_id`
/// is the live FK (`ON DELETE SET NULL` per 0049, so it is nullable on
/// purpose) and goes NULL when the session is deleted. `captured_session_id`
/// has no FK, so nothing un-links it when the session goes — it outlives it.
/// Where its value comes from depends on the row's age: the sink has written
/// it since #695 PR5, and `0049` synthesized it for older rows, which the PR3
/// sink had left NULL. "Captured" is about when the value stops changing, not
/// about which writer put it there.
///
/// For rows written before #695 PR5, `captured_session_id` can also disagree
/// with `payload.session_id` — not with the column beside it, which no writer
/// in this repo's history can make it disagree with: `0049` backfilled the
/// column with the resolved runtime id while the payload still holds the
/// provider's agent session string.
///
/// Both stay `Option<String>` so older fixture rows can still decode in tests
/// that exercise migration boundaries. Plain `String` ids
/// (not the typed `CardId` / `TrackId`) keep the row decode total even for
/// orphaned (`card_id = NULL`) rows.
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
