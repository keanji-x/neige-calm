//! sqlx row wrappers for the calm-types entities (#679 PR1).
//!
//! calm-types is sqlx-free by design (zero-IO compile firewall), and the
//! orphan rule forbids implementing `sqlx::FromRow` for its types from
//! here. So every moved entity gets a thin `XRow` mirror that derives
//! `FromRow` and converts via `From<XRow> for X`:
//!
//! ```text
//!   query_as::<_, WaveRow>(…).fetch_one(…).await?.into()   // → Wave
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

use crate::ids::{CardId, CoveId, WaveId};
use crate::model::{
    Card, Cove, CoveFolder, CoveKind, HarnessItem, Overlay, Wave, WaveLifecycle, WaveWorkspace,
    WaveWorkspaceKind,
};

/// Row mirror of [`Cove`].
#[derive(Debug, sqlx::FromRow)]
pub struct CoveRow {
    #[sqlx(try_from = "String")]
    pub id: CoveId,
    pub name: String,
    pub color: String,
    pub sort: f64,
    #[sqlx(try_from = "String")]
    pub kind: CoveKind,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<CoveRow> for Cove {
    fn from(r: CoveRow) -> Self {
        Cove {
            id: r.id,
            name: r.name,
            color: r.color,
            sort: r.sort,
            kind: r.kind,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Row mirror of [`CoveFolder`].
#[derive(Debug, sqlx::FromRow)]
pub struct CoveFolderRow {
    pub id: i64,
    #[sqlx(try_from = "String")]
    pub cove_id: CoveId,
    pub path: String,
    pub created_at: i64,
}

impl From<CoveFolderRow> for CoveFolder {
    fn from(r: CoveFolderRow) -> Self {
        CoveFolder {
            id: r.id,
            cove_id: r.cove_id,
            path: r.path,
            created_at: r.created_at,
        }
    }
}

/// The `waves` column list every `query_as::<_, WaveRow>` SELECT must use,
/// in `WaveRow` field order.
///
/// Issue #1147 S1: this used to be nine hand-copied literals. `query_as`
/// binds columns by name at **runtime**, so a column added to `WaveRow`
/// without touching all nine SELECTs compiles fine and then blows up in
/// production on whichever route happened to keep the stale list — the
/// `waves` replay of the `CardRow` incident. One const kills the class.
/// Use [`WAVE_SELECT_COLUMNS_W`] where the query aliases the table as `w`.
pub const WAVE_SELECT_COLUMNS: &str = "id, cove_id, title, sort, archived_at, pinned_at, lifecycle, template_id, \
     plugin_scope, purpose, template_input, terminal_at, workspace_kind, workspace_path, \
     workspace_frozen_at, created_at, updated_at";

/// [`WAVE_SELECT_COLUMNS`] with every column qualified by the `w` table alias.
/// `#[sqlx(flatten)]` / `FromRow` still resolve the *unqualified* names, so the
/// two lists must stay in lockstep — `wave_select_columns_lists_agree` pins that.
pub const WAVE_SELECT_COLUMNS_W: &str = "w.id, w.cove_id, w.title, w.sort, w.archived_at, w.pinned_at, w.lifecycle, \
     w.template_id, w.plugin_scope, w.purpose, w.template_input, w.terminal_at, \
     w.workspace_kind, w.workspace_path, w.workspace_frozen_at, w.created_at, w.updated_at";

/// Row mirror of [`Wave`].
#[derive(Debug, sqlx::FromRow)]
pub struct WaveRow {
    #[sqlx(try_from = "String")]
    pub id: WaveId,
    #[sqlx(try_from = "String")]
    pub cove_id: CoveId,
    pub title: String,
    pub sort: f64,
    pub archived_at: Option<i64>,
    pub pinned_at: Option<i64>,
    #[sqlx(try_from = "String")]
    pub lifecycle: WaveLifecycle,
    pub template_id: Option<String>,
    pub plugin_scope: Option<String>,
    pub purpose: Option<String>,
    /// Nullable JSON TEXT column (migration 0061); decodes through the same
    /// `#[sqlx(json)]` machinery as `CardRow.payload`, `nullable` so a NULL
    /// column lands as `None` instead of a decode error.
    #[sqlx(json(nullable))]
    pub template_input: Option<serde_json::Value>,
    pub terminal_at: Option<i64>,
    /// #1147 S1 — migration 0077. The three columns behind [`WaveWorkspace`].
    /// `workspace_path` is the only stored copy of the path; the old `cwd`
    /// column was dropped by that same migration.
    #[sqlx(try_from = "String")]
    pub workspace_kind: WaveWorkspaceKind,
    pub workspace_path: String,
    pub workspace_frozen_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<WaveRow> for Wave {
    fn from(r: WaveRow) -> Self {
        Wave {
            id: r.id,
            cove_id: r.cove_id,
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
            workspace: WaveWorkspace {
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
mod wave_select_columns_tests {
    use super::{WAVE_SELECT_COLUMNS, WAVE_SELECT_COLUMNS_W};

    /// The aliased list must be the unaliased list with `w.` in front of each
    /// name — nothing added, nothing dropped, same order. Without this the two
    /// consts drift and the aliased `wave_detail` SELECT silently loses a
    /// column at runtime, which is the exact failure the consts exist to stop.
    #[test]
    fn wave_select_columns_lists_agree() {
        let plain: Vec<String> = WAVE_SELECT_COLUMNS
            .split(',')
            .map(|c| c.trim().to_string())
            .collect();
        let aliased: Vec<String> = WAVE_SELECT_COLUMNS_W
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
    pub wave_id: WaveId,
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
            wave_id: r.wave_id,
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

/// Row mirror of [`HarnessItem`].
#[derive(Debug, sqlx::FromRow)]
pub struct HarnessItemRow {
    pub id: i64,
    pub runtime_id: String,
    #[sqlx(try_from = "String")]
    pub card_id: CardId,
    #[sqlx(try_from = "String")]
    pub wave_id: WaveId,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub item_uuid: Option<String>,
    pub item_type: Option<String>,
    pub method: String,
    pub params: String,
    pub created_at_ms: i64,
}

impl From<HarnessItemRow> for HarnessItem {
    fn from(r: HarnessItemRow) -> Self {
        HarnessItem {
            id: r.id,
            runtime_id: r.runtime_id,
            card_id: r.card_id,
            wave_id: r.wave_id,
            thread_id: r.thread_id,
            turn_id: r.turn_id,
            item_uuid: r.item_uuid,
            item_type: r.item_type,
            method: r.method,
            params: r.params,
            created_at_ms: r.created_at_ms,
        }
    }
}

/// Row of the `worker_flow_items` table (#695 PR2).
///
/// Sibling of [`HarnessItemRow`], but deliberately *not* a mirror of a
/// calm-types model entity: it is the raw persistence shape for the
/// worker message-flow capture table, returned straight to callers
/// (no `From<…>` projection — PR3's sink/projection owns that).
///
/// `card_id` is `Option<String>` because the table's FK is
/// `REFERENCES cards(id) ON DELETE SET NULL` — a row must survive the
/// deletion of its worker card (#695), so this column goes NULL rather
/// than cascading away. `worker_session_id` is required by migration 0049,
/// and `runtime_id` is populated with the same value by the PR5 sink; the
/// row type keeps these as `Option<String>` so older fixture rows can still
/// decode in tests that exercise migration boundaries. Plain `String` ids
/// (not the typed `CardId` / `WaveId`) keep the row decode total even for
/// orphaned (`card_id = NULL`) rows.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct WorkerFlowItemRow {
    pub id: i64,
    pub card_id: Option<String>,
    pub runtime_id: Option<String>,
    pub wave_id: Option<String>,
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
