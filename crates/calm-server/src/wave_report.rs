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
    WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL, WaveTreeTerm, card_body_crdt_get_tx,
    card_update_with_crdt_tx, deterministic_share, project_tasks_tx,
    project_tasks_with_tree_term_tx, tree_admission_freeze, wave_tree_budget,
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
    let members: Vec<(String, i64, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL)
        .bind(root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut **tx)
        .await?;
    if members.is_empty()
        || members.len() > MAX_TREE_TASK_BUDGET as usize
        || members
            .iter()
            .any(|(_, depth, _)| *depth > MAX_WAVE_TREE_DEPTH)
    {
        return Err(CalmError::Conflict(format!(
            "wave tree rooted at {root_id} is unresolved or exceeds the {MAX_TREE_TASK_BUDGET}-member reprojection bound"
        )));
    }
    let budget = wave_tree_budget(tx, root_id).await?;
    let member_count = members.len() as i64;
    let fixed_live = members
        .iter()
        .map(|(_, _, fixed_live)| *fixed_live)
        .collect::<Vec<_>>();
    let (admission_frozen, minimum_budget_to_unfreeze) = tree_admission_freeze(budget, &fixed_live);
    let mut shares = std::collections::BTreeMap::new();
    let mut projections = Vec::with_capacity(members.len());
    // One member walk above and one grouped postcondition walk below. Member
    // projections must contribute zero because their terms are precomputed.
    let mut tree_cte_queries = 2u32;
    for (index, (member_id, _, _)) in members.into_iter().enumerate() {
        let share = deterministic_share(budget, member_count, index as i64);
        shares.insert(member_id.clone(), share);
        let tree_term = WaveTreeTerm::Share(TreeShare {
            root_id: root_id.to_owned(),
            budget,
            members: member_count,
            member_index: index as i64,
            share,
            admission_frozen,
            minimum_budget_to_unfreeze,
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
use crate::wave_report_guard::{guard_non_prose_stomp, validate_body_fences};
use std::sync::Arc;

// #679 PR1 — `WaveReportPayload` moved to `calm_types::wave_report`
// (Tier-A persisted payload, TS-exported). Re-exported so the
// `crate::wave_report::WaveReportPayload` path is unchanged.
pub use calm_types::wave_report::{ReportBlock, WaveReportPayload};

// ---------------------------------------------------------------------------
// Report-doc operations (#960 PR2)
// ---------------------------------------------------------------------------

/// One mutation of the report's CRDT block map, executed *inside* the
/// persist transaction by [`persist_report_with_shadow`] — the only
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
    guard_task_declarations(&before, &after, author)?;
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
// Shared persist boundary (Issue #247 PR3)
// ---------------------------------------------------------------------------

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
/// pieces `persist_report` needs without duplicating the row-lookup
/// logic across paths. The MCP path uses its own resolver
/// (`mcp_server::tools::wave_report::resolve_report_for_caller`)
/// because it derives the wave from the connection-bound spec card
/// rather than a path parameter — but both ultimately funnel into
/// the same [`persist_report`] writer below.
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

/// Persist a new `WaveReportPayload` onto the wave-report card row and
/// emit `Event::CardUpdated` + `Event::WaveReportEdited` from the same
/// transaction. Returns the updated `Card` row so callers can build
/// their wire response (REST returns the projected payload, MCP
/// returns `{ updated_at }`).
///
/// Single write boundary for every wave-report mutation — both the
/// spec-MCP tools (`calm.report.write` / `calm.report.edit`, with
/// `author = Spec`) and the REST user-edit endpoint (`POST
/// /api/waves/:id/report`, with `author = User`) funnel through this
/// function so the CRDT-write + dual-event invariant holds uniformly:
/// every call → one `CardUpdated` + one `WaveReportEdited`.
///
/// Issue #247 PR1 — materializes the opaque CRDT blob alongside the
/// legacy `payload` JSON. The CRDT is authoritative; the JSON column
/// is a read-cache the existing v1 REST / WS read paths and the
/// frontend continue to consume.
///
/// Issue #247 PR2 — every call also emits a structured
/// `Event::WaveReportEdited` carrying `(summary_before, summary_after,
/// body_before, body_after, author, edit_id)` so PR4's UI can render an
/// edit timeline and PR5's spec agent can wake on user-authored edits.
///
/// Issue #247 PR3 — `author` is now a parameter (was hard-coded
/// `Spec`). The MCP tools pass `EditAuthor::Spec`; the REST handler
/// passes `EditAuthor::User`. The `EditAuthor::Kernel` arm has no
/// caller today and is reserved for future server-internal rewrites.
///
/// In-tx sequence:
///
///   1. Read the current `body_crdt`. NULL = first post-PR1 write on
///      this row (legacy seed / pre-#247 mint); seed a fresh doc from
///      `current_payload`. Non-NULL = load via `ReportDoc::from_bytes`.
///   2. Project the doc to capture `(summary_before, body_before)` —
///      the authoritative pre-write state for the edit-log entry.
///   3. Apply the new `(summary, body)` via `ReportDoc::update` —
///      automerge does the per-field Myers diff internally.
///   4. Project back to `(summary_after, body_after)` and re-serialize
///      a `WaveReportPayload` from those values (not the raw `next`
///      input). The projection is what the JSON cache must mirror so
///      a future read sees the post-merge text rather than a partially-
///      applied input — under single-writer it's identical to `next`,
///      but reading from the doc keeps the JSON-cache contract
///      ("CRDT is source of truth") true by construction.
///   5. Write both columns and emit both events in one tx — via
///      `write_with_events_typed` so the events are persisted in the
///      same transaction as the row update (commit-then-emit invariant
///      preserved).
///
/// **Both events fire on every call, including content-equal writes**
/// (e.g. re-asserting the same body, or `report.edit` with
/// `old_string == new_string`). PR4's UI can filter no-op entries from
/// the timeline if it wants. Keeping the invariant "every
/// `persist_report` call → one `CardUpdated` + one `WaveReportEdited`"
/// dead simple means downstream consumers never have to second-guess
/// whether an event is missing.
///
/// The `current_payload` argument is the payload as it was last seen
/// by the caller. It's used only as the seed for the first-time
/// `from_payload` branch — once `body_crdt` is non-NULL, the doc is
/// the source.
#[allow(clippy::too_many_arguments)]
pub async fn persist_report(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    next: WaveReportPayload,
    if_doc_rev: u64,
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
) -> Result<Card, CalmError> {
    let (updated, _block) = persist_report_with_shadow(
        repo,
        events,
        write,
        actor,
        author,
        wave,
        report_card,
        current_payload,
        ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        },
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
    )
    .await?;
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_report_with_shadow(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    let report_card_id = report_card.id.clone();
    let wave_id = wave.id.clone();
    let cove_id = wave.cove_id.clone();
    let scope = EventScope::Card {
        card: report_card_id.clone(),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let wave_scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id,
    };
    let report_card_id_inner = report_card_id.clone();
    let wave_id_for_event = wave_id.clone();
    let (updated, _ids) = write_with_actor_events_typed::<(Card, Option<BlockOpOutcome>), _>(
        repo,
        None,
        events,
        write,
        move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let wave_id = wave_id_for_event.clone();
            let scope = scope.clone();
            let wave_scope = wave_scope.clone();
            let current_payload = current_payload.clone();
            let op = op.clone();
            let actor = actor.clone();
            let agent_message = agent_message.clone();
            let recorder_shadow = recorder_shadow.clone();
            Box::pin(async move {
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if auto_promote_draft
                    && let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id).await?
                {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &wave_id,
                        target,
                        &actor,
                        agent_message.clone().unwrap_or_default(),
                    )
                    .await?
                {
                    if let Some(probe) = recorder_shadow.as_ref() {
                        probe
                            .record(tx, RecorderShadowDecisionKind::WaveLifecycle)
                            .await?;
                    }
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), wave_scope.clone(), event)),
                    );
                }
                if let Some(probe) = recorder_shadow.as_ref() {
                    probe
                        .record(tx, RecorderShadowDecisionKind::ReportWrite)
                        .await?;
                }
                // 1. Load (or lazy-init) the CRDT doc for this card.
                //    Loaded docs may still carry the pre-#960 layout
                //    (`ROOT.body` Text, no block map) — migrate them
                //    in place, reusing the PR1-derived block ids from
                //    the payload JSON as the id hint. The migrated
                //    bytes are written back below in this same tx.
                let existing = card_body_crdt_get_tx(tx, &id).await?;
                let mut doc = match existing {
                    Some(bytes) => {
                        let mut doc = ReportDoc::from_bytes(&bytes).map_err(|e| {
                            CalmError::Internal(format!(
                                "wave_report: load CRDT for card {id}: {e}"
                            ))
                        })?;
                        doc.ensure_blocks_layout(current_payload.blocks.as_deref())
                            .map_err(|e| {
                                CalmError::Internal(format!(
                                    "wave_report: migrate CRDT block layout for card {id}: {e}"
                                ))
                            })?;
                        doc
                    }
                    // Safe: current_payload was read outside the tx,
                    // but is only consulted here when body_crdt is
                    // still NULL in-tx. SQLite's single-writer means
                    // no concurrent writer can have populated the
                    // blob between that read and this branch; once
                    // body_crdt is non-NULL we take the Some arm and
                    // ignore current_payload entirely.
                    None => ReportDoc::from_payload(&current_payload),
                };
                // 2. Capture the pre-write projection for the edit-log
                //    entry. A malformed doc surfaces as Internal here
                //    (never a panic).
                let (summary_before, body_before) = doc.project().map_err(|e| {
                    CalmError::Internal(format!("wave_report: project CRDT for card {id}: {e}"))
                })?;
                // 3. Apply the requested op on the doc. `if_rev`
                //    checks happen in here, against the CRDT truth
                //    inside this transaction — a conflict aborts the
                //    tx (nothing written, no events emitted).
                let (outcome, doc_rev) = apply_persisted_report_op(&mut doc, &op, author)?;
                // 4. Project back — these are the authoritative values
                //    that go into the JSON cache. Since #960 PR2 the
                //    CRDT block map is the source of truth: `body` is
                //    the per-block concatenation and `blocks` is the
                //    doc's own snapshot (id/rev alignment already
                //    happened inside `ReportDoc::update`), so nothing
                //    is re-derived at the JSON layer.
                let (summary_after, body_after) = doc.project().map_err(|e| {
                    CalmError::Internal(format!(
                        "wave_report: project CRDT post-op for card {id}: {e}"
                    ))
                })?;
                let mut projected_payload =
                    WaveReportPayload::new(summary_after.clone(), body_after.clone());
                projected_payload.doc_rev = doc_rev;
                projected_payload.blocks = Some(doc.blocks_snapshot().map_err(|e| {
                    CalmError::Internal(format!(
                        "wave_report: snapshot CRDT blocks for card {id}: {e}"
                    ))
                })?);
                let blocks = projected_payload.blocks.as_deref().unwrap_or_default();
                let (declarations, block_diagnostics) =
                    calm_types::report_blocks::tasks::project_task_declarations(blocks);
                let payload_value = serde_json::to_value(&projected_payload).map_err(|e| {
                    CalmError::Internal(format!("wave_report: serialize projected payload: {e}"))
                })?;
                let patch = CardPatch {
                    title: None,
                    kind: None,
                    sort: None,
                    payload: Some(payload_value),
                    deletable: None,
                };
                let crdt_bytes = doc.to_bytes();
                // 5. One transactional write rewriting both columns +
                //    two events tagged with the same card scope. Order
                //    matters: `CardUpdated` first so an existing
                //    subscriber that processes both events sees the
                //    generic "row changed" signal before the structured
                //    edit-log entry (matches the historical broadcast
                //    order before PR2 added the structured event).
                let updated = card_update_with_crdt_tx(tx, &id, patch, crdt_bytes).await?;
                // The block-existence leg of projection reads the report cache.
                // Update that cache first inside this same transaction so refs
                // are checked against this write's snapshot, never the previous
                // committed payload. Any projection error still rolls all of it back.
                let task_projection =
                    project_tasks_tx(tx, wave_id.as_str(), &declarations, &block_diagnostics)
                        .await?;
                let report_edited = Event::WaveReportEdited {
                    wave_id: wave_id.clone(),
                    card_id: report_card_id,
                    author,
                    // Kept on the event wire for historical compatibility;
                    // the withdrawn proposal channel has no write point.
                    author_plugin_id: None,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before,
                    summary_after,
                    body_before,
                    body_after,
                    agent_message,
                };
                events.push((
                    actor.clone(),
                    scope.clone(),
                    Event::CardUpdated(updated.clone()),
                ));
                events.push((actor.clone(), scope, report_edited));
                if !task_projection.changed_keys.is_empty() {
                    events.push((
                        actor.clone(),
                        wave_scope,
                        Event::PlanUpdated {
                            wave_id,
                            changed_keys: task_projection.changed_keys,
                            agent_message: None,
                        },
                    ));
                }
                events.extend(task_projection.kernel_events);
                Ok(((updated, outcome), events))
            })
        },
    )
    .await?;
    Ok(updated)
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

    #[test]
    fn initial_uses_current_chinese_seed_body() {
        // Migration 0014 is historical and intentionally remains
        // frozen; freshly-minted waves use the current Chinese seed.
        let p = WaveReportPayload::initial();
        assert_eq!(
            p.body,
            "# 概要\n\n_Spec agent 会在第一次 turn 时填这里。_\n"
        );
    }
}
