//! Issue #229 PR B — wave-report card payload + MCP-tool support helpers.
//!
//! The wave-report card is a kernel-owned card minted at wave-create time
//! (plus backfilled for legacy waves via migration 0014). Its payload is a
//! single Markdown document the spec agent maintains via three MCP tools
//! that mimic codex's native Read/Edit/Write file tools 1:1:
//!
//!   * `calm.report.read`  — fetch current body + summary
//!   * `calm.report.write` — wholesale replace (like codex `Write`)
//!   * `calm.report.edit`  — string replacement (like codex `Edit`;
//!     `old_string` must be unique unless `replace_all = true`)
//!
//! Storage shape is intentionally one big Markdown string rather than a
//! `Vec<Section>` — sections are derived at render time by splitting at
//! H1 headings (`^# `). This keeps the spec agent's mental model simple
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
//! [`crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION`] at every
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
//! (`null` vs missing) for no information gain. `WaveReportPayload::initial()`
//! seeds the canonical "agent hasn't run yet" defaults.

use crate::db::RouteRepo;
use crate::db::sqlite::{
    MAX_TREE_TASK_BUDGET, MAX_WAVE_TREE_DEPTH, TaskProjectionOutcome, TreeShare,
    WAVE_TREE_MEMBERS_SQL, WaveTreeTerm, card_body_crdt_get_tx, card_update_with_crdt_tx,
    deterministic_share, project_tasks_tx, project_tasks_with_tree_term_tx, wave_tree_budget,
    wave_tree_spec_inventory_by_member,
};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardPatch, Wave, WaveLifecycle};
use crate::recorder_shadow::{RecorderShadowDecisionKind, RecorderShadowProbe};
use crate::state::WriteContext;
use crate::wave_lifecycle::{
    apply_requested_transition_in_tx, auto_promote_draft_in_tx, wave_get_tx,
};
use crate::wave_report_doc::ReportDoc;

/// Read the report snapshot from the caller's transaction. A missing report
/// card is an invariant violation: every wave eligible for fork has one.
pub(crate) async fn report_blocks_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wave_id: &str,
) -> crate::error::Result<(String, Vec<ReportBlock>)> {
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(wave_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((payload, body_crdt)) = report else {
        return Err(CalmError::Internal(format!(
            "wave_report: wave {wave_id} is missing its report card"
        )));
    };
    let payload: WaveReportPayload = serde_json::from_str(&payload).map_err(|error| {
        CalmError::Internal(format!(
            "wave_report: decode report payload for fork snapshot: {error}"
        ))
    })?;
    let mut doc = match body_crdt {
        Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(|error| {
            CalmError::Internal(format!(
                "wave_report: load report CRDT for fork snapshot: {error}"
            ))
        })?,
        None => ReportDoc::from_payload(&payload),
    };
    doc.ensure_blocks_layout(payload.blocks.as_deref())
        .map_err(|error| {
            CalmError::Internal(format!(
                "wave_report: migrate report CRDT for fork snapshot: {error}"
            ))
        })?;
    let (summary, _) = doc.project().map_err(|error| {
        CalmError::Internal(format!(
            "wave_report: project report CRDT for fork snapshot: {error}"
        ))
    })?;
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!(
            "wave_report: snapshot report CRDT for fork: {error}"
        ))
    })?;
    Ok((summary, blocks))
}

/// Re-evaluate the task projection from the report CRDT inside the caller's
/// write transaction. `payload` is used only to seed rows whose CRDT has not
/// been initialized yet; once `body_crdt` exists it is the sole source.
pub async fn tasks_rebuild_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wave_id: &str,
) -> crate::error::Result<TaskProjectionOutcome> {
    tasks_rebuild_with_tree_term_tx(tx, wave_id, None).await
}

async fn tasks_rebuild_with_tree_term_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wave_id: &str,
    tree_term: Option<WaveTreeTerm>,
) -> crate::error::Result<TaskProjectionOutcome> {
    let report: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT json(payload),body_crdt FROM cards WHERE wave_id=?1 AND kind='wave-report'",
    )
    .bind(wave_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((payload, body_crdt)) = report else {
        return Ok(TaskProjectionOutcome::default());
    };
    let payload: WaveReportPayload = serde_json::from_str(&payload).map_err(|error| {
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
    Ok(match tree_term {
        Some(tree_term) => {
            project_tasks_with_tree_term_tx(tx, wave_id, &declarations, &diagnostics, tree_term)
                .await?
        }
        None => project_tasks_tx(tx, wave_id, &declarations, &diagnostics).await?,
    })
}

/// Reproject every member after either input to the deterministic quota split
/// changes: the root budget `B` or the member count `N`.
///
/// The recursive member set and budget are read once, then the precomputed
/// [`WaveTreeTerm`] is supplied to each projection. Production admission plus
/// [`MAX_TREE_TASK_BUDGET`] bounds this loop to 64 members. The final grouped
/// inventory check is the transaction's postcondition: pending overage has
/// been culled, and any remaining in-flight overage rejects the B/N change.
pub async fn tasks_rebuild_tree_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root_id: &str,
) -> crate::error::Result<Vec<(Wave, TaskProjectionOutcome)>> {
    let members: Vec<(String, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_SQL)
        .bind(root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    if members.is_empty()
        || members.len() > MAX_TREE_TASK_BUDGET as usize
        || members
            .iter()
            .any(|(_, depth)| *depth > MAX_WAVE_TREE_DEPTH)
    {
        return Err(CalmError::Conflict(format!(
            "wave tree rooted at {root_id} is unresolved or exceeds the {MAX_TREE_TASK_BUDGET}-member reprojection bound"
        )));
    }
    let budget = wave_tree_budget(tx, root_id).await?;
    let member_count = members.len() as i64;
    let mut shares = std::collections::BTreeMap::new();
    let mut projections = Vec::with_capacity(members.len());
    // One member walk above and one grouped postcondition walk below. Member
    // projections must contribute zero because their terms are precomputed.
    let mut tree_cte_queries = 2u32;
    for (index, (member_id, _)) in members.into_iter().enumerate() {
        let share = deterministic_share(budget, member_count, index as i64);
        shares.insert(member_id.clone(), share);
        let tree_term = WaveTreeTerm::Share(TreeShare {
            root_id: root_id.to_owned(),
            budget,
            members: member_count,
            member_index: index as i64,
            share,
            admission_frozen: false,
            minimum_budget_to_unfreeze: None,
        });
        let wave = wave_get_tx(tx, &crate::ids::WaveId::from(member_id.clone())).await?;
        let projection = tasks_rebuild_with_tree_term_tx(tx, &member_id, Some(tree_term)).await?;
        tree_cte_queries = tree_cte_queries.saturating_add(projection.tree_cte_queries);
        projections.push((wave, projection));
    }

    let inventories = wave_tree_spec_inventory_by_member(tx, root_id).await?;
    require_tree_budget_postcondition(root_id, budget, &shares, &inventories)?;
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
            "wave tree change would leave member {member_id} with {live} unfinished spec task(s), above its new share of {share}; wait for in-flight work to finish"
        )));
    }
    if total > budget {
        return Err(CalmError::Conflict(format!(
            "wave tree rooted at {root_id} would hold {total} unfinished spec task(s), above its tree_task_budget of {budget}"
        )));
    }
    Ok(())
}
use crate::wave_report_edit_guard::{guard_task_declarations, normalize_report_op};
use crate::wave_report_guard::{
    guard_non_prose_stomp, validate_block_content, validate_body_fences,
};
use std::sync::Arc;

// #679 PR1 — `WaveReportPayload` moved to `calm_types::wave_report`
// (Tier-A persisted payload, TS-exported). Re-exported so the
// `crate::wave_report::WaveReportPayload` path is unchanged.
pub use calm_types::wave_report::{ReportBlock, WaveReportPayload};

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
/// `CardUpdated` + one `WaveReportEdited` whose `body_before/after`
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
            .map_err(|e| CalmError::Internal(format!("wave_report: block rev: {e}")))?
            .ok_or_else(|| block_not_found(id))?;
        if current != expected {
            return Err(CalmError::Conflict(format!(
                "rev conflict on block {id}: current rev is {current}, expected if_rev {expected} \
                 — re-read the report and retry with the current rev"
            )));
        }
        Ok(current)
    }
    let internal = |e: anyhow::Error| CalmError::Internal(format!("wave_report: block op: {e}"));
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
                    CalmError::Internal(format!("wave_report: read current summary: {e}"))
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
        CalmError::Internal(format!("wave_report: snapshot before task guard: {e}"))
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
                .map_err(|e| CalmError::Internal(format!("wave_report: block rev: {e}")))?
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
    let after = doc
        .blocks_snapshot()
        .map_err(|e| CalmError::Internal(format!("wave_report: snapshot after task guard: {e}")))?;
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
        CalmError::Internal(format!("wave_report: increment document revision: {e}"))
    })?;
    Ok((outcome, doc_rev))
}

fn check_doc_rev(doc: &ReportDoc, expected: u64) -> Result<(), CalmError> {
    let current = doc
        .doc_rev()
        .map_err(|e| CalmError::Internal(format!("wave_report: doc rev: {e}")))?;
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
/// [`resolve_report_for_wave`] together and travel together from there on.
///
/// Introduced by #1318 §1 because the split made the duplication obvious —
/// five signatures (three entry points, the test entry, the writer) each
/// repeated the same three parameters in the same order, which is four chances
/// to transpose two of them. They are one value; this is that value.
///
/// Deliberately not the return type of [`resolve_report_for_wave`] itself: that
/// function is `pub` and its tuple is destructured at ~20 test call sites, and
/// churning those would have buried this slice's actual diff.
///
/// # The fields are private, and the constructor is fallible
///
/// Bundling three parameters into a struct with `pub(crate)` fields would have
/// been the same three parameters with extra steps — worse, actually, because
/// it reads like a checked value while checking nothing. A review channel built
/// the construction: any sibling could write
///
/// ```ignore
/// ReportEditTarget { wave: a, report_card: report_of_b, current_payload: payload_of_b }
/// ```
///
/// and hand it to `write::rest_user_replace`. `write::persist` takes the row id
/// from `report_card` and the event scope, the `PlanUpdated` target and the task
/// reprojection from `wave`, so that call rewrites B's report while emitting A's
/// events and rebuilding A's tasks. The hazard predates this type — the three
/// were independent arguments before — but a type that says "the resolved
/// report a write is about" has to earn the definite article.
///
/// So the fields are visible only inside `wave_report` and its descendants (i.e.
/// this module and `write`), and every other module must go through [`resolve`]
/// or [`for_resolved_parts`], which rejects the mismatch.
///
/// [`resolve`]: ReportEditTarget::resolve
/// [`for_resolved_parts`]: ReportEditTarget::for_resolved_parts
pub(crate) struct ReportEditTarget {
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
}

impl ReportEditTarget {
    /// Resolve by wave id — the REST legs' entry, which had no reason to see the
    /// three parts separately in the first place. Cannot produce a mismatch:
    /// [`resolve_report_for_wave`] finds the report card *among that wave's
    /// cards*.
    pub(crate) async fn resolve(repo: &dyn RouteRepo, id: &str) -> Result<Self, CalmError> {
        let (wave, report_card, current_payload) = resolve_report_for_wave(repo, id).await?;
        Ok(Self {
            wave,
            report_card,
            current_payload,
        })
    }

    /// Build from parts a caller resolved itself — the MCP funnel, whose
    /// resolver (`mcp_server::tools::wave_report::resolve_report_for_caller`)
    /// derives the wave from the connection-bound spec card rather than from a
    /// path parameter.
    ///
    /// Fallible on purpose. The check is one comparison and it is the whole
    /// reason the fields are private: without it this constructor would be a
    /// struct literal wearing a function's clothes.
    pub(crate) fn for_resolved_parts(
        wave: Wave,
        report_card: Card,
        current_payload: WaveReportPayload,
    ) -> Result<Self, CalmError> {
        if report_card.wave_id != wave.id {
            return Err(CalmError::Internal(format!(
                "wave_report: report card {} belongs to wave {}, not {} — a write built from                  these parts would rewrite one wave's report while emitting another's events",
                report_card.id.as_str(),
                report_card.wave_id.as_str(),
                wave.id.as_str()
            )));
        }
        Ok(Self {
            wave,
            report_card,
            current_payload,
        })
    }
}

/// #1318 §1 — the writer and the complete set of ways to reach it.
///
/// The mutating function lives in there as a **private** `fn`, so "which code
/// can write a wave report" is a question `rustc` answers: this module's file
/// and nothing else. Read [`write`]'s header for what that does and does not
/// close. Everything outside calls one of its three `pub(crate)` entry points.
pub(crate) mod write;

/// The pre-#1318 direct handle on the persist boundary, kept for tests only.
///
/// Re-exported here rather than left at `wave_report::write::persist_report`
/// so the ten integration-test files that call
/// `calm_server::wave_report::persist_report` keep working unchanged — the
/// point of this slice is the production caller set, and churning test imports
/// would have buried that diff. Same `cfg` as the definition: absent from any
/// build without `fixtures`, so it is not a hole in the boundary.
#[cfg(any(test, feature = "fixtures"))]
pub use write::persist_report;

/// Look up the wave-report card for a given wave id, returning the
/// `(wave, report_card, current_payload)` triple. The invariant
/// "every wave has exactly one report card" (PR1 backfill + the
/// partial unique index on `cards.kind = 'wave-report'`) means a
/// missing report row signals a data-shape bug, not a 404.
///
/// Errors:
///   * `CalmError::NotFound` — the wave row doesn't exist.
///   * `CalmError::Internal` — wave exists but has no report card
///     (invariant violation), OR the persisted payload won't
///     deserialize (someone wrote past card kind validation).
///
/// Used by `routes::waves::update_wave_report` (REST) to gather the
/// pieces the write entry needs without duplicating the row-lookup
/// logic across paths. The MCP path uses its own resolver
/// (`mcp_server::tools::wave_report::resolve_report_for_caller`)
/// because it derives the wave from the connection-bound spec card
/// rather than a path parameter — but both ultimately funnel into the
/// same `write::persist` writer, through different entry points.
pub async fn resolve_report_for_wave(
    repo: &dyn RouteRepo,
    wave_id: &str,
) -> Result<(Wave, Card, WaveReportPayload), CalmError> {
    let wave = repo
        .wave_get(wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    let cards = repo.cards_by_wave(wave.id.as_str()).await?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "wave-report")
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "wave_report: wave {wave_id} has no wave-report card (invariant violation)"
            ))
        })?;
    let payload: WaveReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            CalmError::Internal(format!(
                "wave_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((wave, report_card, payload))
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
            matches!(error, CalmError::Conflict(message) if message.contains("9 unfinished spec task(s)") && message.contains("tree_task_budget of 8"))
        );
    }

    /// #1318 §1 — the check that makes [`ReportEditTarget`] a resolved target
    /// rather than three arguments in a trench coat.
    ///
    /// The construction a review channel built: hand the constructor wave A
    /// with B's report card, and `write::persist` would rewrite B's row while
    /// emitting A's events and reprojecting A's tasks. Nothing downstream
    /// compares the two, so this is the only place it can be caught — which is
    /// why the fields are private and this is the only door in.
    ///
    /// Both directions, because a constructor that rejected everything would
    /// pass the first assertion on its own.
    #[test]
    fn report_edit_target_pairs_a_card_only_with_its_own_owner() {
        fn parts(card_owner: &str) -> (Wave, Card) {
            let owner = serde_json::from_value(json!({
                "id": "w_a", "area_id": "a_1", "title": "A", "sort": 1.0,
                "archived_at": null, "pinned_at": null, "cwd": "",
                "created_at": 0, "updated_at": 0
            }))
            .expect("owner fixture");
            let card = serde_json::from_value(json!({
                "id": "c_1", "wave_id": card_owner, "kind": "wave-report",
                "sort": 1.0, "payload": {}, "created_at": 0, "updated_at": 0
            }))
            .expect("card fixture");
            (owner, card)
        }

        let (owner, own_card) = parts("w_a");
        ReportEditTarget::for_resolved_parts(owner, own_card, WaveReportPayload::initial())
            .expect("its own report card must build a target");

        let (owner, foreign_card) = parts("w_b");
        // `let ... else` rather than `expect_err`: the latter would need
        // `ReportEditTarget: Debug`, and deriving it to serve one test is how a
        // type grows an accessor nobody asked for.
        let Err(error) =
            ReportEditTarget::for_resolved_parts(owner, foreign_card, WaveReportPayload::initial())
        else {
            panic!("a report card from another owner must not build a target");
        };
        assert!(
            matches!(&error, CalmError::Internal(message)
                if message.contains("belongs to wave w_b, not w_a")),
            "the error must name both so the mismatch is diagnosable, got: {error:?}"
        );
    }

    #[test]
    fn initial_carries_current_schema_version() {
        let p = WaveReportPayload::initial();
        assert_eq!(p.schema_version, WaveReportPayload::SCHEMA_VERSION);
        assert!(p.summary.is_empty());
        assert!(p.body.contains("# 概要"));
        assert!(p.body.ends_with('\n'));
    }

    #[test]
    fn serde_round_trip_camelcase_wire() {
        let p = WaveReportPayload::new("hi", "# A\n\nb\n");
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
        let back: WaveReportPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        // No `body`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
            "schemaVersion": 1,
            "summary": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("body"), "got: {err}");

        // No `summary`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
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
            ReportDoc::from_payload(&WaveReportPayload::new("stale snapshot", "# A\n\nalpha\n"));
        doc.update("racing summary", "# A\n\nalpha\n").unwrap();

        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: "# A\n\nalpha edited\n".into(),
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
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
            EditAuthor::Spec,
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
            EditAuthor::Spec,
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "explicit");
    }

    #[test]
    fn every_report_doc_op_advances_document_revision() {
        fn assert_advances(mut doc: ReportDoc, op: ReportDocOp) {
            let before = doc.doc_rev().unwrap();
            apply_persisted_report_op(&mut doc, &op, EditAuthor::Spec).unwrap();
            assert_eq!(doc.doc_rev().unwrap(), before + 1, "op: {op:?}");
        }

        let payload = WaveReportPayload::new("summary", "# A\n\nalpha\n\n# B\n\nbeta\n");
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
            EditAuthor::Spec,
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
            EditAuthor::Spec,
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");
    }
}
