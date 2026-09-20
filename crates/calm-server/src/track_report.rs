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
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardPatch, Track, TrackLifecycle};
use crate::recorder_shadow::{RecorderShadowDecisionKind, RecorderShadowProbe};
use crate::state::WriteContext;
use crate::track_lifecycle::{
    apply_requested_transition_in_tx, auto_promote_draft_in_tx, track_get_tx,
};
use crate::track_report_doc::ReportDoc;

const REPORT_SNAPSHOT_ROW_SQL: &str =
    "SELECT json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'";

/// Read the report snapshot from the caller's transaction. A missing report
/// card is an invariant violation: every track eligible for fork has one.
pub(crate) async fn report_blocks_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(REPORT_SNAPSHOT_ROW_SQL)
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
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(REPORT_SNAPSHOT_ROW_SQL)
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    report_blocks_snapshot_from_row(track_id, report)
}

/// Shared by the transactional and the autocommit readers above so the two cannot drift.
fn report_blocks_snapshot_from_row(
    track_id: &str,
    report: Option<(String, Option<Vec<u8>>)>,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let Some((payload, body_crdt)) = report else {
        return Err(CalmError::Internal(format!(
            "track_report: track {track_id} is missing its report card"
        )));
    };
    let payload: TrackReportPayload = serde_json::from_str(&payload).map_err(|error| {
        CalmError::Internal(format!(
            "track_report: decode report payload for fork snapshot: {error}"
        ))
    })?;
    let mut doc = match body_crdt {
        Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(|error| {
            CalmError::Internal(format!(
                "track_report: load report CRDT for fork snapshot: {error}"
            ))
        })?,
        None => ReportDoc::from_payload(&payload),
    };
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
        let track = track_get_tx(tx, &crate::ids::TrackId::from(member_id.clone())).await?;
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
    guard_non_prose_stomp, validate_block_content, validate_body_fences,
};
use calm_types::report_blocks::KIND_PROSE;
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
    /// Wholesale `(summary, body)` replace. `summary: None` keeps the doc's CURRENT summary,
    /// resolved inside the persist transaction (an outside-tx snapshot would let a concurrent
    /// summary write be silently reverted).
    Replace {
        summary: Option<String>,
        body: String,
        if_doc_rev: u64,
    },
    /// `calm.report.write_markdown`: wholesale replace whose body may carry `<!-- neige:b_xxxx -->`
    /// marker lines, stripped in-tx and used as exact id-reuse hints. `summary: None` keeps the current summary.
    WriteMarkdown {
        summary: Option<String>,
        body: String,
        if_doc_rev: u64,
    },
    /// `calm.report.blocks.upsert`. `id: None` creates and requires `if_doc_rev`; `id: Some`
    /// replaces and requires `if_rev`. `content` is the block's flat text.
    UpsertBlock {
        id: Option<String>,
        kind: String,
        content: String,
        if_rev: Option<u32>,
        if_doc_rev: Option<u64>,
        position: Option<usize>,
    },
    /// `calm.report.blocks.move`: reorder only, rev untouched; requires `if_doc_rev` because it mutates block order.
    MoveBlock {
        id: String,
        to_index: usize,
        if_doc_rev: u64,
    },
    /// `calm.report.blocks.delete`: `if_rev` is mandatory.
    DeleteBlock { id: String, if_rev: u32 },
    /// `calm.report.commit`: an ordered list of block ops + optional summary under ONE `if_doc_rev`.
    /// A failure anywhere aborts the whole persist transaction; the doc rev advances exactly once.
    /// A `Delete` inside a batch carries no live-task exemption (only the single `DeleteBlock` may retire one).
    Batch {
        if_doc_rev: u64,
        summary: Option<String>,
        ops: Vec<BatchBlockOp>,
    },
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
/// current revisions (block and document) so the RPC mapping can hand a retry without a re-read.
fn check_rev(doc: &ReportDoc, id: &str, expected: u32) -> Result<u32, CalmError> {
    // A malformed doc/rev is Internal (corruption), never folded into "block not found".
    let current = doc
        .block_rev(id)
        .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
        .ok_or_else(|| block_not_found(id))?;
    if current != expected {
        let doc_rev = doc
            .doc_rev()
            .map_err(|e| CalmError::Internal(format!("track_report: doc rev: {e}")))?;
        return Err(CalmError::Conflict(format!(
            "rev conflict on block {id}: current rev is {current}, expected if_rev {expected}; \
             current doc_rev is {doc_rev} — re-read the report and retry with the current rev"
        )));
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
) -> Result<BlockOpOutcome, CalmError> {
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
fn apply_move(doc: &mut ReportDoc, id: &str, to_index: usize) -> Result<BlockOpOutcome, CalmError> {
    let current = doc
        .block_rev(id)
        .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
        .ok_or_else(|| block_not_found(id))?;
    let len = doc.block_index().map_err(block_op_internal)?.len();
    if to_index >= len {
        return Err(CalmError::BadRequest(format!(
            "to_index {to_index} out of range (report has {len} blocks)"
        )));
    }
    doc.move_block(id, to_index).map_err(block_op_internal)?;
    Ok(BlockOpOutcome {
        id: id.to_string(),
        rev: current,
    })
}

fn apply_delete(doc: &mut ReportDoc, id: &str, if_rev: u32) -> Result<(), CalmError> {
    check_rev(doc, id, if_rev)?;
    doc.delete_block(id).map_err(block_op_internal)
}

/// Upper bound on the ops one `calm.report.commit` may carry.
pub const MAX_BATCH_OPS: usize = 64;

/// The block-outcome half of [`apply_report_op_traced`], for the in-crate guard tests.
#[cfg(test)]
pub(crate) fn apply_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<Option<BlockOpOutcome>, CalmError> {
    apply_report_op_traced(doc, op, author).map(|trace| trace.block)
}

/// [`apply_report_op`] plus the written-prose trace.
pub(crate) fn apply_report_op_traced(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<ReportOpTrace, CalmError> {
    let internal = block_op_internal;
    // `summary: None` = keep the current summary, resolved HERE from the doc itself — never from a
    // caller-side snapshot, which could revert a summary written since the caller's read.
    let tx_summary = |doc: &ReportDoc, summary: &Option<String>| -> Result<String, CalmError> {
        match summary {
            Some(summary) => Ok(summary.clone()),
            None => Ok(doc
                .project()
                .map_err(|e| {
                    CalmError::Internal(format!("track_report: read current summary: {e}"))
                })?
                .0),
        }
    };
    // The content rule judges what a CALLER sent, read before `normalize_report_op` rewrites a
    // user's `DeleteBlock` on a live task into a server-synthesized tombstone upsert: a schema
    // check over those bytes would make the repair path fail on the very data it exists to retire.
    let caller_block_content = match op {
        ReportDocOp::UpsertBlock { kind, content, .. } => Some((kind.as_str(), content.as_str())),
        _ => None,
    };
    let op = normalize_report_op(doc, op.clone(), author)?;
    let before = doc.blocks_snapshot().map_err(|e| {
        CalmError::Internal(format!("track_report: snapshot before task guard: {e}"))
    })?;
    let mut written = Written::None;
    let outcome: Result<Option<BlockOpOutcome>, CalmError> = match &op {
        ReportDocOp::Replace {
            summary,
            body,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            let summary = tx_summary(doc, summary)?;
            // Line 1 is rewritten to the canonical header before anything reads the body, so every check
            // and the doc write see the same bytes the funnel will.
            let body = normalize_header(body).map_err(header_bad_request)?;
            validate_body_fences(&body)?;
            guard_non_prose_stomp(doc, &body)?;
            doc.update(&summary, &body).map_err(internal)?;
            written = Written::AllProse;
            Ok(None)
        }
        ReportDocOp::WriteMarkdown {
            summary,
            body,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            let summary = tx_summary(doc, summary)?;
            let marked = calm_types::report_blocks::strip_markers_and_split(body);
            // Normalize AFTER the markers are stripped: a `with_markers` read puts the marker on line 1
            // and the header on line 2. Replacing one comment line by another cannot change the block
            // count, so the hints stay index-aligned.
            let cleaned = normalize_header(&marked.cleaned).map_err(header_bad_request)?;
            let rebuilt;
            let slices = match &cleaned {
                Cow::Borrowed(_) => &marked.slices,
                Cow::Owned(cleaned) => {
                    rebuilt = calm_types::report_blocks::split_body(cleaned);
                    if rebuilt.len() != marked.hints.len() {
                        return Err(CalmError::Internal(format!(
                            "track_report: normalizing the contract header changed the block \
                             count ({} → {})",
                            marked.hints.len(),
                            rebuilt.len()
                        )));
                    }
                    &rebuilt
                }
            };
            // The escape hatch MAY rewrite/delete non-prose blocks, but every fence it carries must be
            // well-formed and schema-valid.
            validate_body_fences(&cleaned)?;
            doc.update_with_hints(&summary, slices, &marked.hints)
                .map_err(internal)?;
            written = Written::AllProse;
            Ok(None)
        }
        ReportDocOp::UpsertBlock {
            id,
            kind,
            content,
            if_rev,
            if_doc_rev,
            position,
        } => {
            // Defence in depth at the op layer: `ReportDoc::upsert_block` only asks `parse_fence` + a kind
            // match, so without this a fence could land in a `prose` block and a schema-invalid payload
            // in a data block. The synthesized tombstone is not judged here (see `caller_block_content`).
            let validate = caller_block_content.is_some();
            let outcome = match id {
                Some(id) => {
                    let expected = if_rev.ok_or_else(|| {
                        CalmError::BadRequest(
                            "if_rev is required when replacing an existing block".into(),
                        )
                    })?;
                    apply_upsert_existing(doc, id, kind, content, expected, validate)
                }
                None => {
                    let expected = if_doc_rev.ok_or_else(|| {
                        CalmError::BadRequest("if_doc_rev is required when creating a block".into())
                    })?;
                    check_doc_rev(doc, expected)?;
                    apply_upsert_new(doc, kind, content, *position, validate)
                }
            };
            outcome.map(|outcome| {
                if kind == KIND_PROSE {
                    written = Written::Ids(vec![outcome.id.clone()]);
                }
                Some(outcome)
            })
        }
        ReportDocOp::MoveBlock {
            id,
            to_index,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            apply_move(doc, id, *to_index).map(Some)
        }
        ReportDocOp::DeleteBlock { id, if_rev } => apply_delete(doc, id, *if_rev).map(|()| None),
        ReportDocOp::Batch {
            if_doc_rev,
            summary,
            ops,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            if ops.len() > MAX_BATCH_OPS {
                return Err(CalmError::BadRequest(format!(
                    "batch carries {} ops; at most {MAX_BATCH_OPS} per commit",
                    ops.len()
                )));
            }
            // Ops run in order against the doc as the previous ones left it; batches address existing
            // blocks (a created id is not known to the caller yet). The first `?` aborts the whole tx.
            let mut written_ids = Vec::new();
            for (index, block_op) in ops.iter().enumerate() {
                let step = |e: CalmError| match e {
                    CalmError::Conflict(m) => CalmError::Conflict(format!("ops[{index}]: {m}")),
                    CalmError::BadRequest(m) => CalmError::BadRequest(format!("ops[{index}]: {m}")),
                    other => other,
                };
                match block_op {
                    BatchBlockOp::Upsert {
                        id,
                        kind,
                        content,
                        if_rev,
                        position,
                    } => {
                        let outcome = match id {
                            Some(id) => {
                                let expected = if_rev.ok_or_else(|| {
                                    CalmError::BadRequest(
                                        "if_rev is required when replacing an existing block"
                                            .into(),
                                    )
                                })?;
                                apply_upsert_existing(doc, id, kind, content, expected, true)
                                    .map_err(step)?
                            }
                            None => apply_upsert_new(doc, kind, content, *position, true)
                                .map_err(step)?,
                        };
                        if kind == KIND_PROSE {
                            written_ids.push(outcome.id);
                        }
                    }
                    BatchBlockOp::Move { id, to_index } => {
                        apply_move(doc, id, *to_index).map_err(step)?;
                    }
                    BatchBlockOp::Delete { id, if_rev } => {
                        apply_delete(doc, id, *if_rev).map_err(step)?;
                    }
                }
            }
            if let Some(summary) = summary {
                doc.set_summary(summary).map_err(internal)?;
            }
            if !written_ids.is_empty() {
                written = Written::Ids(written_ids);
            }
            Ok(None)
        }
    };
    let outcome = outcome?;
    let after = doc.blocks_snapshot().map_err(|e| {
        CalmError::Internal(format!("track_report: snapshot after task guard: {e}"))
    })?;
    let written_prose_block_ids = match written {
        Written::None => Vec::new(),
        Written::Ids(ids) => ids,
        Written::AllProse => after
            .iter()
            .filter(|block| block.kind == KIND_PROSE)
            .map(|block| block.id.clone())
            .collect(),
    };
    // The block-level delete endpoint is the ONLY way a live task declaration may leave the
    // document. `op` is the normalized op, so a user delete rewritten into a tombstone grants no exemption.
    let block_delete_id = match &op {
        ReportDocOp::DeleteBlock { id, .. } => Some(id.as_str()),
        _ => None,
    };
    guard_task_declarations(&before, &after, author, block_delete_id)?;
    Ok(ReportOpTrace {
        block: outcome,
        written_prose_block_ids,
    })
}

/// Advance the document-wide revision exactly once per successful op. Kept outside
/// [`apply_report_op`]: every operation invalidates whole-document anchors, including moves
/// and content-equal replacements that do not bump a block.
fn apply_persisted_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<(ReportOpTrace, u64), CalmError> {
    let trace = apply_report_op_traced(doc, op, author)?;
    let doc_rev = doc.increment_doc_rev().map_err(|e| {
        CalmError::Internal(format!("track_report: increment document revision: {e}"))
    })?;
    Ok((trace, doc_rev))
}

fn check_doc_rev(doc: &ReportDoc, expected: u64) -> Result<(), CalmError> {
    let current = doc
        .doc_rev()
        .map_err(|e| CalmError::Internal(format!("track_report: doc rev: {e}")))?;
    if current != expected {
        return Err(CalmError::Conflict(format!(
            "document revision conflict: current doc_rev is {current}, expected if_doc_rev {expected} \
             — re-read the report and retry with the current docRev"
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

pub(crate) mod dispatch;
mod repair;
mod user_start;
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whole_tree_total_postcondition_rejects_an_over_budget_inventory() {
        // Feed a deliberately inconsistent share map to prove the independent fail-closed guard
        // remains live even if the share construction is corrupted.
        let shares = std::collections::BTreeMap::from([("root".to_owned(), 9)]);
        let error =
            require_tree_budget_postcondition("root", 8, &shares, &[("root".to_owned(), 9)])
                .unwrap_err();
        assert!(
            matches!(error, CalmError::Conflict(message) if message.contains("9 unfinished planner task(s)") && message.contains("tree_task_budget of 8"))
        );
    }

    /// Both directions, because a constructor that rejected everything would pass the first assertion on its own.
    #[test]
    fn report_edit_target_pairs_a_card_only_with_its_own_owner() {
        fn parts(card_owner: &str) -> (Track, Card) {
            let owner = serde_json::from_value(json!({
                "id": "w_a", "area_id": "a_1", "title": "A", "sort": 1.0,
                "archived_at": null, "pinned_at": null, "cwd": "",
                "created_at": 0, "updated_at": 0
            }))
            .expect("owner fixture");
            let card = serde_json::from_value(json!({
                "id": "c_1", "track_id": card_owner, "kind": "track-report",
                "sort": 1.0, "payload": {}, "created_at": 0, "updated_at": 0
            }))
            .expect("card fixture");
            (owner, card)
        }

        let (owner, own_card) = parts("w_a");
        ReportEditTarget::for_resolved_parts(owner, own_card, TrackReportPayload::initial())
            .expect("its own report card must build a target");

        let (owner, foreign_card) = parts("w_b");
        // `let ... else` rather than `expect_err`: the latter would need `ReportEditTarget: Debug`.
        let Err(error) = ReportEditTarget::for_resolved_parts(
            owner,
            foreign_card,
            TrackReportPayload::initial(),
        ) else {
            panic!("a report card from another owner must not build a target");
        };
        assert!(
            matches!(&error, CalmError::Internal(message)
                if message.contains("belongs to track w_b, not w_a")),
            "the error must name both so the mismatch is diagnosable, got: {error:?}"
        );
    }

    #[test]
    fn initial_carries_current_schema_version() {
        let p = TrackReportPayload::initial();
        assert_eq!(p.schema_version, TrackReportPayload::SCHEMA_VERSION);
        assert!(p.summary.is_empty());
        assert!(p.body.contains("# 概要"));
        assert!(p.body.ends_with('\n'));
    }

    #[test]
    fn serde_round_trip_camelcase_wire() {
        let p = TrackReportPayload::new("hi", "# A\n\nb\n");
        let v = serde_json::to_value(&p).unwrap();
        // Wire shape: camelCase keys. A drift here would break the frontend's zod schema silently.
        assert_eq!(
            v,
            json!({
                "schemaVersion": 4,
                "docRev": 0,
                "summary": "hi",
                "body": "# A\n\nb\n",
            })
        );
        let back: TrackReportPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        // No `body`.
        let err = serde_json::from_value::<TrackReportPayload>(json!({
            "schemaVersion": 1,
            "summary": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("body"), "got: {err}");

        // No `summary`.
        let err = serde_json::from_value::<TrackReportPayload>(json!({
            "schemaVersion": 1,
            "body": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("summary"), "got: {err}");
    }

    #[test]
    fn apply_op_with_none_summary_resolves_from_doc_inside_tx() {
        // `summary: None` must resolve against the doc — the in-tx truth — not any caller-side
        // snapshot. Simulate the race by moving the doc's summary after "the caller read it".
        let mut doc =
            ReportDoc::from_payload(&TrackReportPayload::new("stale snapshot", "# A\n\nalpha\n"));
        doc.update("racing summary", "# A\n\nalpha\n").unwrap();

        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: "# A\n\nalpha edited\n".into(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap();
        assert!(outcome.is_none());
        let (summary, body) = doc.project().unwrap();
        assert_eq!(
            summary, "racing summary",
            "None must keep the doc's current (in-tx) summary"
        );
        assert_eq!(body, "# A\n\nalpha edited\n");

        // Replace with None behaves identically; Some overrides.
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: "# B\n\nbeta\n".into(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "racing summary");
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: Some("explicit".into()),
                body: "# C\n\ngamma\n".into(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "explicit");
    }

    #[test]
    fn every_report_doc_op_advances_document_revision() {
        fn assert_advances(mut doc: ReportDoc, op: ReportDocOp) {
            let before = doc.doc_rev().unwrap();
            apply_persisted_report_op(&mut doc, &op, EditAuthor::Planner).unwrap();
            assert_eq!(doc.doc_rev().unwrap(), before + 1, "op: {op:?}");
        }

        let payload = TrackReportPayload::new("summary", "# A\n\nalpha\n\n# B\n\nbeta\n");
        let base = ReportDoc::from_payload(&payload);
        let blocks = base.block_index().unwrap();
        let first = blocks[0].clone();
        let second = blocks[1].clone();

        // Content-equal replace is deliberately included: it is a document write even when no block revision changes.
        assert_advances(
            ReportDoc::from_payload(&payload),
            ReportDocOp::Replace {
                summary: Some(payload.summary.clone()),
                body: payload.body.clone(),
                if_doc_rev: 0,
            },
        );
        assert_advances(
            ReportDoc::from_payload(&payload),
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: payload.body.clone(),
                if_doc_rev: 0,
            },
        );
        assert_advances(
            ReportDoc::from_payload(&payload),
            ReportDocOp::UpsertBlock {
                id: Some(first.0.clone()),
                kind: "prose".into(),
                content: "# A\n\nchanged\n".into(),
                if_rev: Some(first.2),
                if_doc_rev: None,
                position: None,
            },
        );
        assert_advances(
            ReportDoc::from_payload(&payload),
            ReportDocOp::MoveBlock {
                id: first.0.clone(),
                to_index: 1,
                if_doc_rev: 0,
            },
        );
        assert_advances(
            ReportDoc::from_payload(&payload),
            ReportDocOp::DeleteBlock {
                id: second.0,
                if_rev: second.2,
            },
        );
    }

    #[test]
    fn apply_op_on_malformed_doc_is_internal_not_bad_request() {
        use automerge::transaction::Transactable;
        use automerge::{AutoCommit, ObjType, ROOT};

        // Shape 1: block rev stored as a Str. Must surface Internal, never "block not found".
        let mut raw = AutoCommit::new();
        let summary_id = raw.put_object(&ROOT, "summary", ObjType::Text).unwrap();
        raw.update_text(&summary_id, "s").unwrap();
        let blocks = raw.put_object(&ROOT, "blocks", ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, "kind", "prose").unwrap();
        raw.put(&entry, "rev", "three").unwrap();
        let text_id = raw.put_object(&entry, "text", ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        let order = raw.put_object(&ROOT, "order", ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let mut doc = ReportDoc::from_bytes(&raw.save()).unwrap();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some("b_0001".into()),
                kind: "prose".into(),
                content: "x\n".into(),
                if_rev: Some(1),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");

        // Shape 2: blocks map present but no order list. The corrupt doc must not be read as an empty
        // report and silently overwritten.
        let mut raw = AutoCommit::new();
        let summary_id = raw.put_object(&ROOT, "summary", ObjType::Text).unwrap();
        raw.update_text(&summary_id, "s").unwrap();
        raw.put_object(&ROOT, "blocks", ObjType::Map).unwrap();
        let mut doc = ReportDoc::from_bytes(&raw.save()).unwrap();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: Some("s".into()),
                body: String::new(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");
    }

    // The contract-header ingress rules at the op layer; where a header may SIT is the funnel's
    // question and lives with the persist-path tests.

    use calm_types::report_contract::{
        ContractHeader, ContractSection, HEADER_OPEN, canonical_line,
    };

    /// A one-section header as a caller might spell it: keys out of declaration order and an
    /// explicit `"omit_if_empty":false`, both of which the canonical form drops.
    const NON_CANONICAL_HEADER: &str = "<!-- neige:contract {\"sections\":[{\"omit_if_empty\":false,\"h1\":\"概要\"}],\"version\":1} -->";

    fn one_section_header() -> ContractHeader {
        ContractHeader {
            version: 1,
            sections: vec![ContractSection {
                h1: "概要".into(),
                omit_if_empty: false,
            }],
        }
    }

    fn first_line(body: &str) -> &str {
        body.split('\n').next().unwrap_or_default()
    }

    #[test]
    fn replace_normalizes_a_non_canonical_header_line() {
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let rest = "\n\n# 概要\n\nwritten\n";
        assert_ne!(
            NON_CANONICAL_HEADER,
            canonical_line(&one_section_header()),
            "the fixture must be non-canonical or this test proves nothing"
        );
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: format!("{NON_CANONICAL_HEADER}{rest}"),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .expect("a non-canonical header is normalized, not refused");
        let (_, body) = doc.project().unwrap();
        assert_eq!(first_line(&body), canonical_line(&one_section_header()));
        assert_eq!(
            &body[first_line(&body).len()..],
            rest,
            "every byte after line 1 is untouched"
        );
    }

    #[test]
    fn replace_with_a_malformed_header_is_bad_request_and_leaves_the_doc_unchanged() {
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let before = doc.project().unwrap();
        let err = apply_persisted_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: format!(
                    "{HEADER_OPEN}{{\"version\":1,\"sections\":[{{\"h1\":\"-->\"}}]}} -->\n\n# A\n"
                ),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.starts_with("report contract header: ")),
            "got {err:?}"
        );
        assert_eq!(
            doc.project().unwrap(),
            before,
            "a refused write lands nothing"
        );
        assert_eq!(doc.doc_rev().unwrap(), 0, "and advances nothing");
    }

    /// A `with_markers` read puts the marker on line 1 and the header on line 2, so normalizing
    /// before the strip would see a marker, not a header.
    #[test]
    fn write_markdown_normalizes_the_header_after_stripping_markers() {
        let canonical = canonical_line(&one_section_header());
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new(
            "s",
            format!("{canonical}\n\n# 概要\n\nalpha\n"),
        ));
        let index = doc.block_index().unwrap();
        assert_eq!(index.len(), 2, "contract block + one section");
        let (contract_id, section_id) = (index[0].0.clone(), index[1].0.clone());

        let marked = format!(
            "<!-- neige:{contract_id} -->\n{NON_CANONICAL_HEADER}\n\n<!-- neige:{section_id} -->\n# 概要\n\nalpha edited\n"
        );
        apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: marked,
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .expect("write_markdown with markers and a non-canonical header");
        let (_, body) = doc.project().unwrap();
        assert_eq!(
            first_line(&body),
            canonical,
            "line 1 canonical after the strip"
        );
        assert!(
            !body.contains("<!-- neige:b_"),
            "markers never reach storage: {body:?}"
        );
        let after = doc.block_index().unwrap();
        assert_eq!(
            after
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect::<Vec<_>>(),
            [contract_id.as_str(), section_id.as_str()],
            "the rebuilt slices were paired with the original hints"
        );
        assert_eq!(after[1].2, 2, "the edited section bumped its rev");
    }

    /// The doc here has no header, so the resulting document is one the funnel accepts; the
    /// "doc already had one" half is a persist-path test.
    #[test]
    fn upsert_prose_at_position_0_carrying_a_header_is_normalized() {
        let canonical = canonical_line(&one_section_header());

        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: None,
                kind: KIND_PROSE.into(),
                content: format!("{NON_CANONICAL_HEADER}\n"),
                if_rev: None,
                if_doc_rev: Some(0),
                position: Some(0),
            },
            EditAuthor::Planner,
        )
        .expect("single upsert");
        let (_, body) = doc.project().unwrap();
        assert_eq!(first_line(&body), canonical, "single op: {body:?}");

        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        apply_report_op(
            &mut doc,
            &ReportDocOp::Batch {
                if_doc_rev: 0,
                summary: None,
                ops: vec![BatchBlockOp::Upsert {
                    id: None,
                    kind: KIND_PROSE.into(),
                    content: format!("{NON_CANONICAL_HEADER}\n"),
                    if_rev: None,
                    position: Some(0),
                }],
            },
            EditAuthor::Planner,
        )
        .expect("batch upsert");
        let (_, body) = doc.project().unwrap();
        assert_eq!(first_line(&body), canonical, "batch step: {body:?}");

        // A malformed header is refused at the same ingress, with the step index the batch prefixes.
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch {
                if_doc_rev: 0,
                summary: None,
                ops: vec![BatchBlockOp::Upsert {
                    id: None,
                    kind: KIND_PROSE.into(),
                    content: format!("{HEADER_OPEN}not json -->\n"),
                    if_rev: None,
                    position: Some(0),
                }],
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m)
                if m.starts_with("ops[0]: report contract header: ")),
            "got {err:?}"
        );
    }

    /// A stale anchor is judged before the content, so stale `if_rev` / `if_doc_rev` plus a
    /// malformed header is still a `Conflict`.
    #[test]
    fn a_stale_rev_beside_a_malformed_header_is_still_a_conflict() {
        let malformed = format!("{HEADER_OPEN}not json -->\n");
        let payload = TrackReportPayload::new("s", "# A\n\nalpha\n");
        let mut doc = ReportDoc::from_payload(&payload);
        let (id, _, rev) = doc.block_index().unwrap()[0].clone();

        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some(id.clone()),
                kind: KIND_PROSE.into(),
                content: malformed.clone(),
                if_rev: Some(rev + 1),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(err, CalmError::Conflict(_)),
            "replace arm: {err:?}"
        );

        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: None,
                kind: KIND_PROSE.into(),
                content: malformed.clone(),
                if_rev: None,
                if_doc_rev: Some(7),
                position: Some(0),
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Conflict(_)), "create arm: {err:?}");

        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch {
                if_doc_rev: 0,
                summary: None,
                ops: vec![BatchBlockOp::Upsert {
                    id: Some(id),
                    kind: KIND_PROSE.into(),
                    content: malformed,
                    if_rev: Some(rev + 1),
                    position: None,
                }],
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::Conflict(m) if m.starts_with("ops[0]: ")),
            "batch step: {err:?}"
        );
        assert_eq!(
            doc.project().unwrap(),
            ("s".to_string(), "# A\n\nalpha\n".to_string())
        );
    }
}
