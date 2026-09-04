//! Issue #229 PR B — track-report card payload + MCP-tool support helpers.
//!
//! The track-report card is a kernel-owned card minted at track-create time
//! (plus backfilled for legacy tracks via migration 0014). Its payload is a
//! single Markdown document the planner agent maintains via three MCP tools
//! that mimic codex's native Read/Edit/Write file tools 1:1:
//!
//!   * `calm.report.read`  — fetch current body + summary
//!   * `calm.report.write` — wholesale replace (like codex `Write`)
//!   * `calm.report.edit`  — string replacement (like codex `Edit`;
//!     `old_string` must be unique unless `replace_all = true`)
//!
//! Storage shape is intentionally one big Markdown string rather than a
//! `Vec<Section>` — sections are derived at render time by splitting at
//! H1 headings (`^# `). This keeps the planner agent's mental model simple
//! (it's editing a Markdown file), keeps the wire shape stable across
//! UI iterations on the section vocabulary, and avoids a second
//! storage-shape negotiation if the section list ever needs to change.
//!
//! The persisted payload has this wire shape:
//!
//! | Field | Meaning |
//! | --- | --- |
//! | `schemaVersion` | Tier-A payload schema version |
//! | `docRev` | optimistic-concurrency anchor returned by `calm.report.read` |
//! | `summary` | short report summary |
//! | `body` | complete Markdown document |
//! | `blocks` | optional derived block index |
//!
//! ## Schema versioning (Tier A persistence contract)
//!
//! See `docs/upgrade-stability.md`. The struct carries `schema_version`
//! explicitly + matches it against
//! [`crate::validation::TRACK_REPORT_PAYLOAD_SCHEMA_VERSION`] at every
//! write boundary. The current shape is v3 (`docRev` + optional block
//! index). During a downgrade window, an old binary can overwrite the JSON
//! payload back to v2, drop `docRev`, and fail to advance CRDT `doc_rev`;
//! mixed-version report writes therefore have a real lost-write window and
//! are unsupported.
//!
//! ## Field rationale ([[required-over-option]])
//!
//! `summary` and `body` are required `String` (not `Option<String>`):
//! every callsite must commit to a value. An empty `summary` is a valid
//! value ("the agent hasn't written a one-liner yet"); the `Option`
//! shape would have introduced two indistinguishable absent-states
//! (`null` vs missing) for no information gain. `TrackReportPayload::initial()`
//! seeds the canonical "agent hasn't run yet" defaults.

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

/// Read the report snapshot from the caller's transaction. A missing report
/// card is an invariant violation: every track eligible for fork has one.
pub(crate) async fn report_blocks_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT json(payload),body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
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

async fn task_projection_source_tx(
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

/// Strictly reproject every member after the root budget `B` is edited or a
/// member is added, increasing `N`.
///
/// The recursive member set and budget are read once, then the precomputed
/// [`TrackTreeTerm`] is supplied to each projection. Production admission plus
/// [`MAX_TREE_TASK_BUDGET`] bounds this loop to 64 members. The final grouped
/// inventory check is the transaction's postcondition: pending overage has
/// been culled, and any remaining in-flight overage rejects that tightening.
/// Member removal uses [`tasks_rebuild_tree_after_member_removal_tx`] because a
/// pre-existing frozen overage must not make an otherwise safe deletion fail.
pub async fn tasks_rebuild_tree_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root_id: &str,
) -> crate::error::Result<Vec<(Track, TaskProjectionOutcome)>> {
    tasks_rebuild_tree_with_policy_tx(tx, root_id, TreeRebuildPolicy::Strict).await
}

/// Reproject a tree after one member has already been removed in this
/// transaction. Unlike an operator-requested budget/member addition, deletion
/// may safely preserve an existing in-flight overage: N only fell, no fixed
/// work was added, and admission remains frozen until that work terminates.
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
use std::sync::Arc;

// #679 PR1 — `TrackReportPayload` moved to `calm_types::track_report`
// (Tier-A persisted payload, TS-exported). Re-exported so the
// `crate::track_report::TrackReportPayload` path is unchanged.
pub use calm_types::track_report::{ReportBlock, TrackReportPayload};

// ---------------------------------------------------------------------------
// Report-doc operations (#960 PR2)
// ---------------------------------------------------------------------------

/// One mutation of the report's CRDT block map, executed *inside* the
/// persist transaction by `write::persist` — the only
/// place `if_rev` may be checked, because only there is
/// `ReportDoc::block_rev` the transactional truth (the JSON `blocks`
/// cache can be arbitrarily stale under D8).
///
/// Every variant lands through the same five-step persist sequence and
/// therefore keeps the dual-event invariant: one successful op = one
/// `CardUpdated` + one `TrackReportEdited` whose `body_before/after`
/// are the flat projections.
#[derive(Debug, Clone)]
pub enum ReportDocOp {
    /// Wholesale `(summary, body)` replace — the legacy
    /// `calm.report.write`/`edit` tools and the REST user-edit path.
    /// `summary: None` keeps the doc's **current** summary, resolved
    /// inside the persist transaction against the CRDT truth (#960
    /// PR2 review: an outside-tx snapshot would let a concurrent
    /// summary write be silently reverted — TOCTOU).
    Replace {
        summary: Option<String>,
        body: String,
        if_doc_rev: u64,
    },
    /// `calm.report.write_markdown`: wholesale replace whose body may
    /// carry `<!-- neige:b_xxxx -->` marker lines. Markers are
    /// stripped unconditionally in-tx (they never reach storage) and
    /// become exact id-reuse hints; unmarked slices fall back to the
    /// LCS alignment. `summary: None` keeps the current summary,
    /// resolved in-tx (same TOCTOU rule as [`Self::Replace`]).
    WriteMarkdown {
        summary: Option<String>,
        body: String,
        if_doc_rev: u64,
    },
    /// `calm.report.blocks.upsert`. `id: None` creates (at `position`,
    /// default append) and requires `if_doc_rev`; `id: Some` replaces
    /// and requires `if_rev`.
    /// `content` is the block's flat text: markdown for `prose`, the
    /// canonical `neige-block` fence (already rendered + validated by
    /// the tool layer) for data kinds (#960 PR3).
    UpsertBlock {
        id: Option<String>,
        kind: String,
        content: String,
        if_rev: Option<u32>,
        if_doc_rev: Option<u64>,
        position: Option<usize>,
    },
    /// `calm.report.blocks.move`: reorder only, rev untouched; requires
    /// the document-wide `if_doc_rev` because it mutates block order.
    MoveBlock {
        id: String,
        to_index: usize,
        if_doc_rev: u64,
    },
    /// `calm.report.blocks.delete`: `if_rev` is mandatory.
    DeleteBlock { id: String, if_rev: u32 },
}

/// `(id, rev)` a block-level [`ReportDocOp`] resolved to: the created/
/// replaced/moved block's id and its post-op rev. `None` for the
/// wholesale and delete variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockOpOutcome {
    pub id: String,
    pub rev: u32,
}

pub(crate) fn block_not_found(id: &str) -> CalmError {
    CalmError::BadRequest(format!("block {id} not found"))
}

/// Execute `op` against the (already migrated) doc. Returns
/// `CalmError::Conflict` on an `if_rev` mismatch — the persist closure
/// propagates it, aborting the transaction, so a conflicting op writes
/// nothing and emits nothing. Unknown ids / out-of-range indexes are
/// `CalmError::BadRequest`.
pub(crate) fn apply_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<Option<BlockOpOutcome>, CalmError> {
    fn check_rev(doc: &ReportDoc, id: &str, expected: u32) -> Result<u32, CalmError> {
        // A malformed doc/rev is Internal (corruption), never folded
        // into "block not found" (BadRequest).
        let current = doc
            .block_rev(id)
            .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
            .ok_or_else(|| block_not_found(id))?;
        if current != expected {
            return Err(CalmError::Conflict(format!(
                "rev conflict on block {id}: current rev is {current}, expected if_rev {expected} \
                 — re-read the report and retry with the current rev"
            )));
        }
        Ok(current)
    }
    let internal = |e: anyhow::Error| CalmError::Internal(format!("track_report: block op: {e}"));
    // `summary: None` = keep the current summary. Resolved HERE,
    // inside the persist transaction, from the doc itself — never from
    // a caller-side snapshot (which could revert a summary written
    // between the caller's read and this tx).
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
    // #1269 (+ follow-up) — the content rule judges what a CALLER sent,
    // so the `kind`/`content` it sees are read off the caller's own op,
    // here, before `normalize_report_op` below can hand the `UpsertBlock`
    // arm something else. That rewrite turns a user's `DeleteBlock` on a
    // live task into a tombstone upsert the server synthesizes from
    // fields already stored on that block; running a schema check over
    // those bytes would make a *repair* path fail on the very data it
    // exists to retire (a task stored with a `key` the current schema
    // rejects could no longer be deleted at all, since the whole-document
    // shapes refuse to drop a live task). `render_fence`'s output is
    // still parsed and kind-matched by `ReportDoc::upsert_block`; only
    // the payload-schema gate is scoped to caller bytes.
    //
    // This is `Some` exactly when the caller's op is an `UpsertBlock`,
    // and `normalize_report_op` returns such an op unchanged (it rewrites
    // only `DeleteBlock`), so the `UpsertBlock` arms below run with this
    // `Some` for every caller-supplied upsert. Reading it here rather
    // than checking before the match keeps the existing order of
    // verdicts: a stale `if_rev` is still the `Conflict` it was.
    let caller_block_content = match op {
        ReportDocOp::UpsertBlock { kind, content, .. } => Some((kind.as_str(), content.as_str())),
        _ => None,
    };
    let op = normalize_report_op(doc, op.clone(), author)?;
    let before = doc.blocks_snapshot().map_err(|e| {
        CalmError::Internal(format!("track_report: snapshot before task guard: {e}"))
    })?;
    let outcome: Result<Option<BlockOpOutcome>, CalmError> = match &op {
        ReportDocOp::Replace {
            summary,
            body,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            let summary = tx_summary(doc, summary)?;
            validate_body_fences(body)?;
            guard_non_prose_stomp(doc, body)?;
            doc.update(&summary, body).map_err(internal)?;
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
            // The escape hatch MAY rewrite/delete non-prose blocks
            // (that is its point), but every fence it carries must be
            // well-formed and schema-valid — reject the whole write
            // otherwise (#960 PR3).
            validate_body_fences(&marked.cleaned)?;
            doc.update_with_hints(&summary, &marked.slices, &marked.hints)
                .map_err(internal)?;
            Ok(None)
        }
        ReportDocOp::UpsertBlock {
            id,
            kind,
            content,
            if_rev,
            if_doc_rev,
            position,
        } => match id {
            Some(id) => {
                let expected = if_rev.ok_or_else(|| {
                    CalmError::BadRequest(
                        "if_rev is required when replacing an existing block".into(),
                    )
                })?;
                check_rev(doc, id, expected)?;
                // #1269 (+ follow-up) — defence in depth at the op
                // layer, on both halves of `kind`. All `ReportDoc::
                // upsert_block` asks of the content is `parse_fence` +
                // a kind match, so a direct `apply_report_op` call used
                // to carry a ```neige-block fence straight into a
                // `kind: "prose"` block, and a schema-invalid payload
                // straight into a data block. Content a user sends to
                // the block *upsert* endpoints (MCP #971 / REST #990)
                // never arrives that way — they run
                // `check_prose_markdown` on a prose argument and build
                // data content with `render_data_block` — and the point
                // is that the op stops depending on them to do so.
                // `caller_block_content` is read before the delete
                // rewrite, so the tombstone that rewrite synthesizes is
                // not judged here; see its comment above. Which rule
                // each `kind` gets and what is left to `upsert_block`
                // (and so still surfaces as a 500 rather than a 400) is
                // written up once on `validate_block_content` rather
                // than restated here.
                if let Some((kind, content)) = caller_block_content {
                    validate_block_content(kind, content)?;
                }
                let (id, rev) = doc
                    .upsert_block(Some(id), kind, content)
                    .map_err(internal)?;
                Ok(Some(BlockOpOutcome { id, rev }))
            }
            None => {
                let expected = if_doc_rev.ok_or_else(|| {
                    CalmError::BadRequest("if_doc_rev is required when creating a block".into())
                })?;
                check_doc_rev(doc, expected)?;
                // #1269 (+ follow-up) — same check on the create arm;
                // leaving either arm unchecked would leave the op-layer
                // gap open. (The delete rewrite only ever produces the
                // replace arm above, since it carries the stored block's
                // id, so this arm sees caller content in every case.)
                if let Some((kind, content)) = caller_block_content {
                    validate_block_content(kind, content)?;
                }
                let len = doc.block_index().map_err(internal)?.len();
                if let Some(position) = position
                    && *position > len
                {
                    return Err(CalmError::BadRequest(format!(
                        "position {position} out of range (report has {len} blocks)"
                    )));
                }
                let (id, rev) = doc.upsert_block(None, kind, content).map_err(internal)?;
                if let Some(position) = position
                    && *position < len
                {
                    doc.move_block(&id, *position).map_err(internal)?;
                }
                Ok(Some(BlockOpOutcome { id, rev }))
            }
        },
        ReportDocOp::MoveBlock {
            id,
            to_index,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            let current = doc
                .block_rev(id)
                .map_err(|e| CalmError::Internal(format!("track_report: block rev: {e}")))?
                .ok_or_else(|| block_not_found(id))?;
            let len = doc.block_index().map_err(internal)?.len();
            if *to_index >= len {
                return Err(CalmError::BadRequest(format!(
                    "to_index {to_index} out of range (report has {len} blocks)"
                )));
            }
            doc.move_block(id, *to_index).map_err(internal)?;
            Ok(Some(BlockOpOutcome {
                id: id.clone(),
                rev: current,
            }))
        }
        ReportDocOp::DeleteBlock { id, if_rev } => {
            check_rev(doc, id, *if_rev)?;
            doc.delete_block(id).map_err(internal)?;
            Ok(None)
        }
    };
    let outcome = outcome?;
    let after = doc.blocks_snapshot().map_err(|e| {
        CalmError::Internal(format!("track_report: snapshot after task guard: {e}"))
    })?;
    // The block-level delete endpoint is the ONLY way a live task
    // declaration may leave the document (#1179); the guard needs to know
    // which block, if any, this op deleted that way. `op` here is the
    // normalized op, so a user delete (rewritten into an in-place
    // tombstone) is not a delete anymore and grants no exemption.
    let block_delete_id = match &op {
        ReportDocOp::DeleteBlock { id, .. } => Some(id.as_str()),
        _ => None,
    };
    guard_task_declarations(&before, &after, author, block_delete_id)?;
    Ok(outcome)
}

/// Apply one successful persist operation and advance the document-wide
/// revision exactly once. Keeping the increment outside [`apply_report_op`]
/// is load-bearing: every operation invalidates whole-document anchors,
/// including moves and content-equal replacements that do not bump a block.
fn apply_persisted_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<(Option<BlockOpOutcome>, u64), CalmError> {
    let outcome = apply_report_op(doc, op, author)?;
    let doc_rev = doc.increment_doc_rev().map_err(|e| {
        CalmError::Internal(format!("track_report: increment document revision: {e}"))
    })?;
    Ok((outcome, doc_rev))
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

// ---------------------------------------------------------------------------
// Shared persist boundary (Issue #247 PR3, closed by #1318 §1)
// ---------------------------------------------------------------------------

/// The resolved report a write is about: the three values that come out of
/// [`resolve_report_for_track`] together and travel together from there on.
///
/// Introduced by #1318 §1 because the split made the duplication obvious —
/// five signatures (three entry points, the test entry, the writer) each
/// repeated the same three parameters in the same order, which is four chances
/// to transpose two of them. They are one value; this is that value.
///
/// Deliberately not the return type of [`resolve_report_for_track`] itself: that
/// function is `pub` and its tuple is destructured at ~20 test call sites, and
/// churning those would have buried this slice's actual diff.
///
/// # The fields are private, and the constructor catches an accidental mismatch
///
/// Read the next two paragraphs together; the second is the one that keeps this
/// doc honest.
///
/// **What the constructor is for.** `write::persist` takes the row id from
/// `report_card`, and the event scope, the `PlanUpdated` target and the task
/// reprojection from `track`. Hand it a mismatched pair and it rewrites B's
/// report while emitting A's events and rebuilding A's tasks — and nothing
/// downstream compares the two. A review channel built exactly that from a
/// struct literal, back when the fields were `pub(crate)`. So the fields are
/// private (visible only inside `track_report` and its descendants), and every
/// other module goes through [`resolve`] — which cannot mismatch, because
/// `resolve_report_for_track` finds the card *among that track's cards* — or
/// through [`for_resolved_parts`], which compares the pair.
///
/// **What that comparison is worth.** Not much, against a caller that means it.
/// `Card::track_id` is a `pub` field, so a sibling holding B's real card can
/// clone it, overwrite `track_id` with A's, and walk straight through: the
/// check compares two values the caller supplied. It is a **drift catch for an
/// accidental pairing, not a guard** — the same reason this slice refused a
/// witness token minted from `Actor` (`write.rs`, "What is still not closed",
/// item 2), and it would be inconsistent to ship the shape here under a better
/// name. A real check has to run against the row inside the write transaction;
/// see item 6 of that list.
///
/// `current_payload` is not checked at all. It seeds the CRDT on a first write
/// (`body_crdt` still NULL) and supplies the block-id hints on layout
/// migration, so a wrong one is a real defect — it simply has no cheap local
/// comparison, since a payload carries no owner.
///
/// [`resolve`]: ReportEditTarget::resolve
/// [`for_resolved_parts`]: ReportEditTarget::for_resolved_parts
pub(crate) struct ReportEditTarget {
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
}

impl ReportEditTarget {
    /// Resolve by track id — the REST legs' entry, which had no reason to see the
    /// three parts separately in the first place. Cannot produce a mismatch:
    /// [`resolve_report_for_track`] finds the report card *among that track's
    /// cards*.
    pub(crate) async fn resolve(repo: &dyn RouteRepo, id: &str) -> Result<Self, CalmError> {
        let (track, report_card, current_payload) = resolve_report_for_track(repo, id).await?;
        Ok(Self {
            track,
            report_card,
            current_payload,
        })
    }

    /// Build from parts a caller resolved itself — the MCP funnel, whose
    /// resolver (`mcp_server::tools::track_report::resolve_report_for_caller`)
    /// derives the track from the connection-bound spec card rather than from a
    /// path parameter.
    ///
    /// Fallible on purpose. The check is one comparison and it is the whole
    /// reason the fields are private: without it this constructor would be a
    /// struct literal wearing a function's clothes.
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

/// #1318 §1 — the writer and the complete set of ways to reach it.
///
/// The mutating function lives in there as a **private** `fn`, so "which code
/// can write a track report" is a question `rustc` answers: this module's file
/// and nothing else. Read [`write`]'s header for what that does and does not
/// close. Everything outside calls one of its three `pub(crate)` entry points.
pub(crate) mod write;

/// The pre-#1318 direct handle on the persist boundary, kept for tests only.
///
/// Re-exported here rather than left at `track_report::write::persist_report`
/// so the ten integration-test files that call
/// `calm_server::track_report::persist_report` keep working unchanged — the
/// point of this slice is the production caller set, and churning test imports
/// would have buried that diff. Same `cfg` as the definition: absent from any
/// build without `fixtures`, so it is not a hole in the boundary.
#[cfg(any(test, feature = "fixtures"))]
pub use write::persist_report;

/// Look up the track-report card for a given track id, returning the
/// `(track, report_card, current_payload)` triple. The invariant
/// "every track has exactly one report card" (PR1 backfill + the
/// partial unique index on `cards.kind = 'track-report'`) means a
/// missing report row signals a data-shape bug, not a 404.
///
/// Errors:
///   * `CalmError::NotFound` — the track row doesn't exist.
///   * `CalmError::Internal` — track exists but has no report card
///     (invariant violation), OR the persisted payload won't
///     deserialize (someone wrote past card kind validation).
///
/// Used by `routes::tracks::update_track_report` (REST) to gather the
/// pieces the write entry needs without duplicating the row-lookup
/// logic across paths. The MCP path uses its own resolver
/// (`mcp_server::tools::track_report::resolve_report_for_caller`)
/// because it derives the track from the connection-bound planner card
/// rather than a path parameter — but both ultimately funnel into the
/// same `write::persist` writer, through different entry points.
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
        // Exact production share construction makes member-overage imply this
        // branch. Feed a deliberately inconsistent share map to prove the
        // independent fail-closed guard remains live if that construction is
        // ever corrupted without changing the grouped inventory.
        let shares = std::collections::BTreeMap::from([("root".to_owned(), 9)]);
        let error =
            require_tree_budget_postcondition("root", 8, &shares, &[("root".to_owned(), 9)])
                .unwrap_err();
        assert!(
            matches!(error, CalmError::Conflict(message) if message.contains("9 unfinished planner task(s)") && message.contains("tree_task_budget of 8"))
        );
    }

    /// #1318 §1 — the check that makes [`ReportEditTarget`] a resolved target
    /// rather than three arguments in a trench coat.
    ///
    /// The construction a review channel built: hand the constructor track A
    /// with B's report card, and `write::persist` would rewrite B's row while
    /// emitting A's events and reprojecting A's tasks. Nothing downstream
    /// compares the two, so this is the only place it can be caught — which is
    /// why the fields are private and this is the only door in.
    ///
    /// Both directions, because a constructor that rejected everything would
    /// pass the first assertion on its own.
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
        // `let ... else` rather than `expect_err`: the latter would need
        // `ReportEditTarget: Debug`, and deriving it to serve one test is how a
        // type grows an accessor nobody asked for.
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
        // Wire shape: camelCase keys. A drift here would break the
        // frontend's zod schema silently — pin via this test.
        assert_eq!(
            v,
            json!({
                "schemaVersion": 3,
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
        // #960 PR2 review (write_markdown TOCTOU): `summary: None`
        // must resolve against the doc — the in-tx truth — not any
        // caller-side snapshot. Simulate the race by moving the doc's
        // summary after "the caller read it".
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

        // Content-equal replace is deliberately included: it is a document
        // write even when no block revision changes.
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

        // Shape 1: block rev stored as a Str. check_rev must surface
        // CalmError::Internal (corruption), never fold the broken rev
        // into "block not found" (BadRequest).
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

        // Shape 2: blocks map present but no order list. A wholesale
        // replace must fail Internal-level — the corrupt doc must not
        // be read as an empty report and silently overwritten.
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
}
