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
use crate::model::{Card, Cove, CoveFolder, CoveKind, HarnessItem, Overlay, Wave, WaveLifecycle};

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
    pub cwd: String,
    pub terminal_at: Option<i64>,
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
            cwd: r.cwd,
            terminal_at: r.terminal_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Row mirror of [`Card`].
///
/// `Card.runtime` is `#[sqlx(skip)]` in spirit: it is a lazy projection
/// joined after the fetch (`runtime_get_projectable_for_card`), never a
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
/// than cascading away. `runtime_id` / `wave_id` / `worker_session_id`
/// are nullable for the same forward-compatibility reasons the DDL
/// documents. Plain `String` ids (not the typed `CardId` / `WaveId`)
/// keep the row decode total even for orphaned (`card_id = NULL`) rows.
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
