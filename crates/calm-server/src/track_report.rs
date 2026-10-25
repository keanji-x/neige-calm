//! Track-report card payload + MCP-tool support helpers. The payload is one Markdown document
//! (sections derived at render time by splitting at H1); mixed-version report writes across a
//! downgrade window have a real lost-write window and are unsupported.

use crate::db::RouteRepo;
use crate::db::sqlite::{
    MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TRACK_TREE_MEMBERS_SQL,
    TRACK_TREE_MEMBERS_WITH_FIXED_PLANNER_SQL, TaskProjectionOutcome, TrackTreeTerm, TreeShare,
    card_body_crdt_get_tx, card_update_with_crdt_tx, deterministic_share, project_tasks_tx,
    project_tasks_with_tree_term_tx, track_tree_budget, track_tree_planner_inventory_by_member,
    tree_share_from_member_inventory,
};
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardPatch, Track};
use crate::recorder_admission::{RecorderAdmissionDecisionKind, RecorderAdmissionGate};
use crate::state::WriteContext;
use crate::track_report_doc::ReportDoc;
use calm_types::report_blocks::tasks::normalize_legacy_task_blocks;

const REPORT_SNAPSHOT_ROW_SQL: &str = "SELECT json(payload),body_crdt IS NOT NULL FROM cards \
     WHERE track_id=?1 AND kind='track-report'";

/// Read the report snapshot from the caller's transaction. A missing report
/// card is an invariant violation: every track eligible for fork has one.
pub(crate) async fn report_blocks_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let report: Option<(String, bool)> = sqlx::query_as(REPORT_SNAPSHOT_ROW_SQL)
        .bind(track_id)
        .fetch_optional(&mut **tx)
        .await?;
    report_blocks_snapshot_from_row(track_id, report)
}

/// The same snapshot as ONE autocommit statement on the pool — no transaction: a deferred read
/// transaction holding R locks across statements against an IMMEDIATE writer deadlocks.
pub(crate) async fn report_blocks_snapshot(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let report: Option<(String, bool)> = sqlx::query_as(REPORT_SNAPSHOT_ROW_SQL)
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    report_blocks_snapshot_from_row(track_id, report)
}

/// Shared by the transactional and the autocommit readers above so the two cannot drift.
fn report_blocks_snapshot_from_row(
    track_id: &str,
    report: Option<(String, bool)>,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let Some((payload, has_crdt)) = report else {
        return Err(CalmError::Internal(format!(
            "track_report: track {track_id} is missing its report card"
        )));
    };
    let payload: TrackReportPayload = serde_json::from_str(&payload).map_err(|error| {
        CalmError::Internal(format!(
            "track_report: decode report payload for fork snapshot: {error}"
        ))
    })?;
    // The only CRDT writer stores the doc's summary and block snapshot in `payload` in the same UPDATE.
    if has_crdt {
        let blocks = payload.blocks.ok_or_else(|| {
            CalmError::Internal(format!(
                "track_report: track {track_id} report CRDT has no stored block projection"
            ))
        })?;
        return Ok((payload.summary, normalize_legacy_task_blocks(&blocks)));
    }
    let mut doc = ReportDoc::from_payload(&payload);
    doc.ensure_blocks_layout(payload.blocks.as_deref())
        .map_err(|error| {
            CalmError::Internal(format!(
                "track_report: migrate report CRDT for fork snapshot: {error}"
            ))
        })?;
    let (summary, _) = doc.project().map_err(|error| {
        CalmError::Internal(format!(
            "track_report: project report CRDT for fork snapshot: {error}"
        ))
    })?;
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!(
            "track_report: snapshot report CRDT for fork: {error}"
        ))
    })?;
    Ok((summary, blocks))
}

/// Re-evaluate the task projection from the report CRDT inside the caller's
/// write transaction. `payload` is used only to seed rows whose CRDT has not
/// been initialized yet; once `body_crdt` exists it is the sole source.
pub async fn tasks_rebuild_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<TaskProjectionOutcome> {
    tasks_rebuild_with_tree_term_tx(tx, track_id, None).await
}

async fn tasks_rebuild_with_tree_term_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    tree_term: Option<TrackTreeTerm>,
) -> crate::error::Result<TaskProjectionOutcome> {
    let Some((declarations, diagnostics)) = task_projection_source_tx(tx, track_id).await? else {
        return Ok(TaskProjectionOutcome::default());
    };
    Ok(match tree_term {
        Some(tree_term) => {
            project_tasks_with_tree_term_tx(tx, track_id, &declarations, &diagnostics, tree_term)
                .await?
        }
        None => project_tasks_tx(tx, track_id, &declarations, &diagnostics).await?,
    })
}

type TaskProjectionSource = (
    Vec<calm_types::report_blocks::tasks::TaskDeclaration>,
    Vec<Vec<calm_types::report_blocks::tasks::Diagnostic>>,
);

pub(crate) async fn task_projection_source_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<Option<TaskProjectionSource>> {
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((payload, body_crdt)) = report else {
        return Ok(None);
    };
    let payload: TrackReportPayload = serde_json::from_str(&payload).map_err(|error| {
        CalmError::Internal(format!("decode report payload for task rebuild: {error}"))
    })?;
    let mut doc = match body_crdt {
        Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(|error| {
            CalmError::Internal(format!("load report CRDT for task rebuild: {error}"))
        })?,
        None => ReportDoc::from_payload(&payload),
    };
    doc.ensure_blocks_layout(payload.blocks.as_deref())
        .map_err(|error| {
            CalmError::Internal(format!("migrate report CRDT for task rebuild: {error}"))
        })?;
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!("snapshot report CRDT for task rebuild: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    Ok(Some((declarations, diagnostics)))
}

/// Validate the exact report/CRDT source a later task rebuild will consume,
/// without changing any task rows. Track deletion runs this for every survivor
/// before it stops runtimes or moves the victim workspace.
pub(crate) async fn validate_task_rebuild_source_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<()> {
    task_projection_source_tx(tx, track_id).await.map(|_| ())
}

/// Strictly reproject every member after the root budget is edited or a member is added. The
/// final grouped inventory check is the postcondition: remaining in-flight overage rejects the
/// tightening. Member removal uses [`tasks_rebuild_tree_after_member_removal_tx`] instead.
pub async fn tasks_rebuild_tree_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root_id: &str,
) -> crate::error::Result<Vec<(Track, TaskProjectionOutcome)>> {
    tasks_rebuild_tree_with_policy_tx(tx, root_id, TreeRebuildPolicy::Strict).await
}

/// Reproject a tree after one member was removed in this transaction; deletion may safely
/// preserve an existing in-flight overage (N only fell, no fixed work was added).
pub async fn tasks_rebuild_tree_after_member_removal_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root_id: &str,
) -> crate::error::Result<Vec<(Track, TaskProjectionOutcome)>> {
    tasks_rebuild_tree_with_policy_tx(tx, root_id, TreeRebuildPolicy::PreserveExistingFreeze).await
}

#[derive(Clone, Copy)]
enum TreeRebuildPolicy {
    Strict,
    PreserveExistingFreeze,
}

async fn tasks_rebuild_tree_with_policy_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root_id: &str,
    policy: TreeRebuildPolicy,
) -> crate::error::Result<Vec<(Track, TaskProjectionOutcome)>> {
    let members: Vec<(String, i64, i64)> = match policy {
        TreeRebuildPolicy::Strict => sqlx::query_as::<_, (String, i64)>(TRACK_TREE_MEMBERS_SQL)
            .bind(root_id)
            .bind(MAX_TRACK_TREE_DEPTH + 1)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|(member_id, depth)| (member_id, depth, 0))
            .collect(),
        TreeRebuildPolicy::PreserveExistingFreeze => {
            sqlx::query_as(TRACK_TREE_MEMBERS_WITH_FIXED_PLANNER_SQL)
                .bind(root_id)
                .bind(MAX_TRACK_TREE_DEPTH + 1)
                .fetch_all(&mut **tx)
                .await?
        }
    };
    if members.is_empty()
        || members.len() > MAX_TREE_TASK_BUDGET as usize
        || members
            .iter()
            .any(|(_, depth, _)| *depth > MAX_TRACK_TREE_DEPTH)
    {
        return Err(CalmError::Conflict(format!(
            "track tree rooted at {root_id} is unresolved or exceeds the {MAX_TREE_TASK_BUDGET}-member reprojection bound"
        )));
    }
    let budget = track_tree_budget(tx, root_id).await?;
    let member_count = members.len() as i64;
    let mut shares = std::collections::BTreeMap::new();
    let mut projections = Vec::with_capacity(members.len());
    // One member walk above and one grouped postcondition walk below. Member
    // projections must contribute zero because their terms are precomputed.
    let mut tree_cte_queries = 2u32;
    let mut admission_frozen = false;
    for (index, (member_id, _, _)) in members.iter().enumerate() {
        let share = deterministic_share(budget, member_count, index as i64);
        shares.insert(member_id.clone(), share);
        let tree_term = match policy {
            TreeRebuildPolicy::Strict => TrackTreeTerm::Share(TreeShare {
                root_id: root_id.to_owned(),
                budget,
                members: member_count,
                member_index: index as i64,
                share,
                admission_frozen: false,
                minimum_budget_to_unfreeze: None,
            }),
            TreeRebuildPolicy::PreserveExistingFreeze => {
                tree_share_from_member_inventory(root_id.to_owned(), member_id, budget, &members)
            }
        };
        admission_frozen |= matches!(
            &tree_term,
            TrackTreeTerm::Share(TreeShare {
                admission_frozen: true,
                ..
            })
        );
        let track =
            calm_truth::db::sqlite::track_get_tx(tx, &crate::ids::TrackId::from(member_id.clone()))
                .await?;
        let projection = tasks_rebuild_with_tree_term_tx(tx, member_id, Some(tree_term)).await?;
        tree_cte_queries = tree_cte_queries.saturating_add(projection.tree_cte_queries);
        projections.push((track, projection));
    }

    let inventories = track_tree_planner_inventory_by_member(tx, root_id).await?;
    if matches!(policy, TreeRebuildPolicy::PreserveExistingFreeze) && admission_frozen {
        let fixed_by_member: std::collections::BTreeMap<_, _> = members
            .iter()
            .map(|(member_id, _, fixed_live)| (member_id.as_str(), *fixed_live))
            .collect();
        if let Some((member_id, live)) = inventories.iter().find(|(member_id, live)| {
            fixed_by_member.get(member_id.as_str()).copied() != Some(*live)
        }) {
            return Err(CalmError::Internal(format!(
                "member-removal reprojection left {live} unfinished planner task(s) on {member_id}, but only {} fixed task(s) may survive admission freeze",
                fixed_by_member
                    .get(member_id.as_str())
                    .copied()
                    .unwrap_or(0)
            )));
        }
    } else {
        require_tree_budget_postcondition(root_id, budget, &shares, &inventories)?;
    }
    if tree_cte_queries != 2 {
        return Err(CalmError::Internal(format!(
            "whole-tree reprojection executed {tree_cte_queries} recursive tree queries; expected exactly 2 independent of member count"
        )));
    }
    Ok(projections)
}

fn require_tree_budget_postcondition(
    root_id: &str,
    budget: i64,
    shares: &std::collections::BTreeMap<String, i64>,
    inventories: &[(String, i64)],
) -> crate::error::Result<()> {
    let total: i64 = inventories.iter().map(|(_, live)| *live).sum();
    let member_overage = inventories
        .iter()
        .find(|(member_id, live)| shares.get(member_id).is_none_or(|share| *live > *share));
    if let Some((member_id, live)) = member_overage {
        let share = shares.get(member_id).copied().unwrap_or(0);
        return Err(CalmError::Conflict(format!(
            "track tree change would leave member {member_id} with {live} unfinished planner task(s), above its new share of {share}; wait for in-flight work to finish"
        )));
    }
    if total > budget {
        return Err(CalmError::Conflict(format!(
            "track tree rooted at {root_id} would hold {total} unfinished planner task(s), above its tree_task_budget of {budget}"
        )));
    }
    Ok(())
}
use crate::track_report_edit_guard::{guard_task_declarations, normalize_report_op};
use crate::track_report_guard::{
    guard_non_prose_stomp, validate_block_content, validate_body_fences, validate_marked_slices,
};
use calm_types::report_blocks::{KIND_PROSE, in_sections};
use calm_types::report_contract::{HeaderError, normalize_header};
use std::borrow::Cow;
use std::sync::Arc;

/// A contract header the caller wrote that does not parse: the ingress rejection.
fn header_bad_request(error: HeaderError) -> CalmError {
    CalmError::BadRequest(format!("report contract header: {error}"))
}

/// Prose content is the only block content that can carry the header line, so it is the only
/// kind normalized at the block ingress (after the rev checks). Data kinds pass through untouched.
fn normalize_prose_content<'a>(kind: &str, content: &'a str) -> Result<Cow<'a, str>, CalmError> {
    if kind == KIND_PROSE {
        normalize_header(content).map_err(header_bad_request)
    } else {
        Ok(Cow::Borrowed(content))
    }
}

pub use calm_types::track_report::{ReportBlock, TrackReportPayload};

/// One mutation of the report's CRDT block map, executed inside the persist transaction — the
/// only place `if_rev` may be checked, because only there is `ReportDoc::block_rev` the
/// transactional truth (the JSON `blocks` cache can be arbitrarily stale).
#[derive(Debug, Clone)]
pub enum ReportDocOp {
    AgentBatch {
        summary: Option<String>,
        ops: Vec<BlockIntent>,
    },
    AgentWrite {
        summary: Option<String>,
        body: String,
    },
    /// The user's wholesale `(summary, body)` replace (REST); the summary is always given.
    Replace {
        summary: String,
        body: String,
        if_doc_rev: u64,
    },
    /// `neige_report_write`: wholesale replace whose body may carry `<!-- neige:b_xxxx -->`
    /// marker lines, stripped in-tx and used as exact id-reuse hints. `summary: None` keeps the current summary.
    /// `if_doc_rev` is the docRev at which the session last read the whole report.
    WriteMarkdown {
        summary: Option<String>,
        body: String,
        if_doc_rev: u64,
    },
    /// The block-level REST upsert. `id: None` creates and requires `if_doc_rev`; `id: Some`
    /// replaces and requires `if_rev`. `content` is the block's flat text.
    UpsertBlock {
        id: Option<String>,
        kind: String,
        content: String,
        if_rev: Option<u32>,
        if_doc_rev: Option<u64>,
        position: Option<usize>,
    },
    /// The block-level REST move: reorder only, rev untouched; requires `if_doc_rev` because it mutates block order.
    MoveBlock {
        id: String,
        to_index: usize,
        if_doc_rev: u64,
    },
    /// The block-level REST delete: `if_rev` is mandatory.
    DeleteBlock { id: String, if_rev: u32 },
    /// Today's Reset: the user asking for [`TrackReportPayload::initial`]. Every non-prose block
    /// counts as deleted by id, the one way a live task may leave the document, and the prose stomp
    /// guard does not apply. No tombstone is left: one would keep the report from being the initial one.
    ResetToInitial { if_doc_rev: u64 },
    /// `neige_report_commit`: an ordered list of block ops + optional summary under ONE document anchor.
    /// A failure anywhere aborts the whole persist transaction; the doc rev advances exactly once.
    /// A `Delete` op may retire a live task it names by id; a section op never reaches a task block.
    Batch {
        doc_anchor: DocAnchor,
        summary: Option<String>,
        ops: Vec<BatchBlockOp>,
    },
}

/// The document-wide anchor of a [`ReportDocOp::Batch`] (#1877).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAnchor {
    /// The `docRev` this session last read: checked only when the batch needs a document anchor
    /// (a `summary`, a created or moved block, a created section), so a section or block edit is not
    /// refused over an unrelated edit elsewhere.
    LastRead(u64),
    /// No read by this session: a batch that needs the anchor is refused.
    Unread,
}

/// What this session last read of one section: its ordered `(id, rev)` list, or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionRead {
    Seen(Vec<(String, u32)>),
    Unseen,
}

/// One step of a [`ReportDocOp::Batch`], minus the document-wide anchor the batch carries once.
#[derive(Debug, Clone)]
pub enum BatchBlockOp {
    /// `id: Some` replaces (needs `if_rev`); `id: None` creates at
    /// `position` (default append).
    Upsert {
        id: Option<String>,
        kind: String,
        content: String,
        if_rev: Option<u32>,
        position: Option<usize>,
    },
    Move {
        id: String,
        to_index: usize,
    },
    Delete {
        id: String,
        if_rev: u32,
    },
    /// `write_markdown` bounded to one H1 section; creates a declared section that is absent.
    ReplaceSection {
        section: String,
        markdown: String,
        read: SectionRead,
    },
    DeleteSection {
        section: String,
        read: SectionRead,
    },
}

/// `(id, rev)` a block-level [`ReportDocOp`] resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockOpOutcome {
    pub id: String,
    pub rev: u32,
}

/// What one [`ReportDocOp`] resolved to, plus the final ids of every prose block the op wrote
/// (the set the receipt's `neige://source/` link warnings scan). A content-equal replace still counts as written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportOpTrace {
    pub block: Option<BlockOpOutcome>,
    pub written_prose_block_ids: Vec<String>,
    /// Content explicitly authored by this caller, independent of intermediate revisions.
    pub written_block_ids: Vec<String>,
    /// Blocks produced by whole-section or whole-document output, never source titles.
    pub section_output_ids: Vec<String>,
    pub knowledge: Option<crate::report_read_ledger::LastRead>,
}

/// Which blocks an op wrote, before the post-op snapshot exists.
enum Written {
    None,
    Ids(Vec<String>),
    /// Every prose block of the post-op document.
    AllProse,
}

pub(crate) fn block_not_found(id: &str) -> CalmError {
    CalmError::BadRequest(format!("block {id} not found"))
}

/// Execute `op` against the (already migrated) doc. `if_rev` mismatch is `Conflict` naming BOTH
/// current revisions (block and document) so callers can locate conflicts before re-reading.
fn check_rev(doc: &ReportDoc, id: &str, expected: u32) -> Result<u32, ReportError> {
    // A malformed doc/rev is Internal (corruption), never folded into "block not found".
    let current = doc
        .block_rev(id)
        .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
        .ok_or_else(|| {
            ReportError::anchor(
                block_not_found(id),
                Reason::ChangedSince,
                Scope::Block(id.into()),
            )
        })?;
    if current != expected {
        let doc_rev = doc
            .doc_rev()
            .map_err(|e| CalmError::Internal(format!("track_report: doc rev: {e}")))?;
        return Err(ReportError::anchor(
            CalmError::Conflict(format!(
                "rev conflict on block {id}: current rev is {current}, expected if_rev {expected}; \
             current doc_rev is {doc_rev} — re-read the affected report text and retry"
            )),
            Reason::ChangedSince,
            Scope::Block(id.into()),
        ));
    }
    Ok(current)
}

fn block_op_internal(e: anyhow::Error) -> CalmError {
    CalmError::Internal(format!("track_report: block op: {e}"))
}

/// Replace an existing block: `if_rev` against the CRDT truth, then the caller-content rule
/// (`validate_caller_content` is false only for the tombstone `normalize_report_op` synthesizes).
fn apply_upsert_existing(
    doc: &mut ReportDoc,
    id: &str,
    kind: &str,
    content: &str,
    expected_rev: u32,
    validate_caller_content: bool,
) -> Result<BlockOpOutcome, ReportError> {
    check_rev(doc, id, expected_rev)?;
    // After the rev check, so a stale `if_rev` is still a `Conflict` even when the content also
    // carries a malformed header.
    let content = normalize_prose_content(kind, content)?;
    if validate_caller_content {
        validate_block_content(kind, &content)?;
    }
    let (id, rev) = doc
        .upsert_block(Some(id), kind, &content)
        .map_err(block_op_internal)?;
    Ok(BlockOpOutcome { id, rev })
}

/// Create a block at `position` (default append). The document-wide anchor is the caller's business.
fn apply_upsert_new(
    doc: &mut ReportDoc,
    kind: &str,
    content: &str,
    position: Option<usize>,
    validate_caller_content: bool,
) -> Result<BlockOpOutcome, CalmError> {
    // The caller has already checked its document-wide anchor, so a stale `if_doc_rev` stays a
    // `Conflict` ahead of a malformed header.
    let content = normalize_prose_content(kind, content)?;
    if validate_caller_content {
        validate_block_content(kind, &content)?;
    }
    let len = doc.block_index().map_err(block_op_internal)?.len();
    if let Some(position) = position
        && position > len
    {
        return Err(CalmError::BadRequest(format!(
            "position {position} out of range (report has {len} blocks)"
        )));
    }
    let (id, rev) = doc
        .upsert_block(None, kind, &content)
        .map_err(block_op_internal)?;
    if let Some(position) = position
        && position < len
    {
        doc.move_block(&id, position).map_err(block_op_internal)?;
    }
    Ok(BlockOpOutcome { id, rev })
}

/// Reorder only; rev untouched.
fn apply_move(
    doc: &mut ReportDoc,
    id: &str,
    to_index: usize,
) -> Result<BlockOpOutcome, ReportError> {
    let current = doc
        .block_rev(id)
        .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
        .ok_or_else(|| {
            ReportError::anchor(
                block_not_found(id),
                Reason::ChangedSince,
                Scope::Block(id.into()),
            )
        })?;
    let len = doc.block_index().map_err(block_op_internal)?.len();
    if to_index >= len {
        return Err(CalmError::BadRequest(format!(
            "to_index {to_index} out of range (report has {len} blocks)"
        ))
        .into());
    }
    doc.move_block(id, to_index).map_err(block_op_internal)?;
    Ok(BlockOpOutcome {
        id: id.to_string(),
        rev: current,
    })
}

fn apply_delete(doc: &mut ReportDoc, id: &str, if_rev: u32) -> Result<(), ReportError> {
    check_rev(doc, id, if_rev)?;
    doc.delete_block(id)
        .map_err(block_op_internal)
        .map_err(Into::into)
}

/// Upper bound on the ops one `neige_report_commit` may carry.
pub const MAX_BATCH_OPS: usize = 64;

mod compact_origination;
#[cfg(test)]
mod compact_tests;
mod ops;
#[cfg(test)]
use ops::apply_persisted_report_op;
#[cfg(test)]
pub(crate) use ops::apply_report_op;

/// A document anchor, checked only when `needed`. `Ok(true)` when the doc is the one the anchor
/// names (checked, or equal though not needed), so the author knew the whole document before this write.
fn check_doc_anchor(doc: &ReportDoc, anchor: DocAnchor, needed: bool) -> Result<bool, ReportError> {
    match anchor {
        DocAnchor::LastRead(read) => {
            let current = doc
                .doc_rev()
                .map_err(|e| CalmError::Internal(format!("track_report: doc rev: {e}")))?;
            if needed && current != read {
                return Err(ReportError::anchor(CalmError::Conflict(format!(
                    "document revision conflict: current doc_rev is {current}, this session last \
                     read doc_rev {read} — re-read the report and retry"
                )), Reason::ChangedSince, Scope::Document));
            }
            Ok(current == read)
        }
        DocAnchor::Unread if needed => Err(ReportError::anchor(CalmError::BadRequest(
            "this session has not read the report's doc_rev: a summary, a created or moved block \
             and a created section need it — read the report with neige_report_read and retry"
                .into(),
        ), Reason::NeverRead, Scope::Document)),
        DocAnchor::Unread => Ok(false),
    }
}

fn check_doc_rev(doc: &ReportDoc, expected: u64) -> Result<(), CalmError> {
    let current = doc
        .doc_rev()
        .map_err(|e| CalmError::Internal(format!("track_report: doc rev: {e}")))?;
    if current != expected {
        return Err(CalmError::Conflict(format!(
            "document revision conflict: current doc_rev is {current}, expected if_doc_rev {expected} \
             — re-read the report text, merge your changes, and retry"
        )));
    }
    Ok(())
}

/// The resolved report a write is about. Fields are private: `write::persist` takes the row id
/// from `report_card` and the events/reprojection from `track`, and a mismatched pair rewrites
/// B's report while emitting A's events. [`for_resolved_parts`] compares the pair — a drift
/// catch for an accidental pairing, not a guard against a caller that means it.
///
/// [`for_resolved_parts`]: ReportEditTarget::for_resolved_parts
#[derive(Clone)]
pub(crate) struct ReportEditTarget {
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
}

impl ReportEditTarget {
    /// Resolve by track id. Cannot produce a mismatch: the report card is found among that track's cards.
    pub(crate) async fn resolve(repo: &dyn RouteRepo, id: &str) -> Result<Self, CalmError> {
        let (track, report_card, current_payload) = resolve_report_for_track(repo, id).await?;
        Ok(Self {
            track,
            report_card,
            current_payload,
        })
    }

    /// Build from parts a caller resolved itself (the MCP funnel derives the track from the
    /// connection-bound spec card). Fallible on purpose: the comparison is the reason the fields are private.
    pub(crate) fn for_resolved_parts(
        track: Track,
        report_card: Card,
        current_payload: TrackReportPayload,
    ) -> Result<Self, CalmError> {
        if report_card.track_id != track.id {
            return Err(CalmError::Internal(format!(
                "track_report: report card {} belongs to track {}, not {} — a write built from                  these parts would rewrite one track's report while emitting another's events",
                report_card.id.as_str(),
                report_card.track_id.as_str(),
                track.id.as_str()
            )));
        }
        Ok(Self {
            track,
            report_card,
            current_payload,
        })
    }
}

#[cfg(test)]
mod reset_tests;
mod sections;
pub(crate) mod task_changes;
/// The writer and the complete set of ways to reach it. The mutating function is a private `fn`
/// in there, so "which code can write a track report" is a question `rustc` answers.
pub(crate) mod write;

/// Direct handle on the persist boundary, kept for tests only; absent from any build without `fixtures`.
#[cfg(any(test, feature = "fixtures"))]
pub use write::persist_report;

/// Look up the track-report card for a track. A missing report row is a data-shape bug
/// (`Internal`), not a 404; `NotFound` only when the track row doesn't exist.
pub async fn resolve_report_for_track(
    repo: &dyn RouteRepo,
    track_id: &str,
) -> Result<(Track, Card, TrackReportPayload), CalmError> {
    let track = repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let cards = repo.cards_by_track(track.id.as_str()).await?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "track-report")
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "track_report: track {track_id} has no track-report card (invariant violation)"
            ))
        })?;
    let payload: TrackReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            CalmError::Internal(format!(
                "track_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((track, report_card, payload))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod objective_tests;

#[cfg(test)]
mod objective_write_tests;

mod intent;
pub use intent::BlockIntent;

pub(crate) mod identity;

impl ReportEditTarget {
    pub(crate) fn into_parts(self) -> (Track, Card, TrackReportPayload) {
        (self.track, self.report_card, self.current_payload)
    }
}

pub mod rejection;
use rejection::{Reason, ReportError, Scope};
