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
//! ## Schema versioning (Tier A persistence contract)
//!
//! See `docs/upgrade-stability.md`. The struct carries `schema_version`
//! explicitly + matches it against
//! [`crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION`] at every
//! write boundary. v1 is the only shape that has ever existed.
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
use crate::db::sqlite::{card_body_crdt_get_tx, card_update_with_crdt_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardPatch, Wave, WaveLifecycle};
use crate::recorder_shadow::{RecorderShadowDecisionKind, RecorderShadowProbe};
use crate::state::WriteContext;
use crate::wave_lifecycle::{apply_requested_transition_in_tx, auto_promote_draft_in_tx};
use crate::wave_report_doc::ReportDoc;
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
    },
    /// `calm.report.blocks.upsert`. `id: None` creates (at `position`,
    /// default append); `id: Some` replaces and requires `if_rev`.
    /// `content` is the block's flat text: markdown for `prose`, the
    /// canonical `neige-block` fence (already rendered + validated by
    /// the tool layer) for data kinds (#960 PR3).
    UpsertBlock {
        id: Option<String>,
        kind: String,
        content: String,
        if_rev: Option<u32>,
        position: Option<usize>,
    },
    /// `calm.report.blocks.move`: reorder only, rev untouched.
    MoveBlock {
        id: String,
        to_index: usize,
        if_rev: Option<u32>,
    },
    /// `calm.report.blocks.delete`: `if_rev` is mandatory.
    DeleteBlock { id: String, if_rev: u32 },
    /// Issue #955 §5.2.2 — transactional batch apply, the proposal
    /// channel's persistence entry. Ops validate-and-apply
    /// **sequentially from one snapshot**: each op is checked against
    /// the document state produced by its predecessors, so a batch can
    /// reference blocks it just created and a contradictory sequence
    /// (delete-then-move) naturally fails. Any failure rolls the WHOLE
    /// batch back — the incoming doc is untouched (the ops run on a
    /// scratch copy that only replaces the real doc on full success)
    /// and the surrounding persist tx aborts. Failure classes split by
    /// what a retry can fix: unknown block ids and rev mismatches are
    /// `CalmError::Conflict` (the proposal-stale class — for an async
    /// proposal "the block you anchored on is gone/moved" is staleness,
    /// fixable against a fresh snapshot), while structural
    /// malformations (invalid member kind, missing `if_rev`,
    /// out-of-range index) stay `BadRequest` — resubmitting the same
    /// shape against any snapshot can never succeed, and PR-b must not
    /// misreport it as stale. A successful batch flows through the
    /// same five-step persist sequence exactly once ⇒ exactly one
    /// `CardUpdated` + one `WaveReportEdited` pair, and the in-op
    /// guards run unchanged inside each constituent op.
    ///
    /// Member kinds are the block-level ops only: nested batches,
    /// `Replace`, and `WriteMarkdown` are refused (`BadRequest`) — the
    /// wire `ProposalOp` cannot express the wholesale writes, and the
    /// kernel-side entry enforces the same ceiling so an op-mapping
    /// bug upstream can never smuggle a full-body rewrite through the
    /// proposal channel. An empty batch is refused too (a proposal
    /// with no ops is a caller bug, not a conflict).
    Batch(Vec<ReportDocOp>),
    /// Issue #955 §5.2.1 (PR-b) — the proposal channel's actual entry:
    /// a [`Batch`](Self::Batch) whose members are the *wire* ops
    /// (`ProposalOp`), lowered against the live scratch document one at
    /// a time.
    ///
    /// Lowering cannot happen outside the persist transaction, which is
    /// why this variant exists next to `Batch` rather than the handler
    /// pre-building one: `temp_id` creations get their durable `b_xxxx`
    /// id **minted by the kernel here** (§5.2.1), and `anchor`s resolve
    /// to list indexes against the document *as the predecessors left
    /// it* — neither is knowable before the ops run. Error classes,
    /// whole-batch rollback and the exactly-one-event-pair invariant are
    /// shared with `Batch` (both funnel through the same classifier and
    /// the same scratch-doc swap).
    ///
    /// `base_doc_heads` is the §5.2.1 whole-document anchor. It is
    /// authoritative **only** for creations with no block-level anchor
    /// (`at_start` / `at_end`): those have nothing else to anchor on, so
    /// any committed change since the plugin's snapshot makes the batch
    /// stale. For every other op it is advisory — the per-block `if_rev`s
    /// carry the authority, which is exactly what lets a proposal survive
    /// spec edits to *unrelated* blocks.
    ProposalBatch {
        base_doc_heads: String,
        ops: Vec<calm_types::proposal::ProposalOp>,
    },
}

/// Issue #955 §5.6 (PR-b) — accept-time coupling: the pending proposal
/// whose resolution must commit in the SAME write transaction as its
/// [`ReportDocOp::ProposalBatch`] apply, so no crash window can produce
/// "resolved but not written" or "written but still pending".
///
/// Presence of this context turns [`persist_report_with_shadow`] into
/// the accept path: it re-checks the proposal is still pending (and
/// still owned by `plugin_id` on this wave) inside the tx, prepends the
/// `ProposalResolved { accepted }` event (actor `User` — the human is
/// the adjudicator, §5.4), and stamps the plugin attribution onto the
/// `WaveReportEdited` the apply emits (`author = "plugin"` +
/// `author_plugin_id`, §5.3).
#[derive(Debug, Clone)]
pub struct ProposalAcceptCtx {
    pub proposal_id: String,
    /// The SUBMITTING plugin — both the attribution carried into
    /// `WaveReportEdited.author_plugin_id` and the in-tx ownership bind
    /// on the resolve event's payload.
    pub plugin_id: String,
}

/// Exact message of the in-tx "not pending any more" conflict, so
/// [`classify_accept_error`] can tell a genuine double-resolve (→ 409)
/// apart from an anchoring conflict (→ resolve as `stale`). Pinned by
/// `accept_error_classes_are_disjoint`.
pub(crate) const PROPOSAL_NOT_PENDING: &str = "proposal is no longer pending";

/// Prefix every anchoring/staleness conflict minted inside a batch
/// carries. Also pinned by `accept_error_classes_are_disjoint`.
pub(crate) const PROPOSAL_BATCH_CONFLICT: &str = "proposal batch conflict: ";

/// What went wrong in an accept transaction (§5.6). Note what is NOT
/// here any more: staleness. A failed anchor is not an error, it is the
/// `stale` verdict, and since the verdict is only authoritative in the
/// transaction that determined it, that transaction records it itself and
/// returns [`ProposalAcceptOutcome::Stale`].
#[derive(Debug)]
pub enum AcceptFailure {
    /// The proposal was resolved by someone else between the pre-flight
    /// and this transaction ⇒ 409.
    NotPending,
    /// Anything else (structural `BadRequest`, `NotFound`, `Internal`) —
    /// surfaced verbatim; the proposal stays pending.
    Other(CalmError),
}

/// How an accept transaction ended (§5.6). Both variants COMMITTED: the
/// verdict and its (absence of) report change are one transaction.
// One-shot return value, moved straight into the response builder and
// dropped — never stored or collected, so the size gap between "here is
// the updated card" and "here is why nothing changed" costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ProposalAcceptOutcome {
    /// Every anchoring check passed: the report changed and
    /// `ProposalResolved { accepted }` + `CardUpdated` +
    /// `WaveReportEdited` committed together.
    Accepted(Card),
    /// An anchoring check failed inside the accept transaction. That
    /// SAME transaction appended `ProposalResolved { stale }` and left
    /// the report untouched (no `CardUpdated`, no `WaveReportEdited`, the
    /// card row and CRDT blob never written). Carries the conflict
    /// message for the response body.
    ///
    /// Resolving in-tx is not a nicety: it closes the race where the
    /// determination is made, the transaction rolls back, and a
    /// concurrent reject/withdraw commits before a second transaction
    /// can record the stale verdict — turning an already-authoritatively
    /// -stale accept into a 409 or a different terminal decision.
    Stale(String),
}

/// Classify the error a proposal-accept [`persist_report_with_shadow`]
/// call returned. Anchoring conflicts never reach here — they are
/// resolved inside the transaction. Structural `BadRequest`s
/// deliberately do NOT become `stale` either: per §5.2.1's error split
/// they can never succeed against any snapshot, so staling them would
/// hide an unfixable proposal behind a "retry against a fresh baseline"
/// verdict.
pub fn classify_accept_error(e: CalmError) -> AcceptFailure {
    match e {
        CalmError::Conflict(msg) if msg == PROPOSAL_NOT_PENDING => AcceptFailure::NotPending,
        other => AcceptFailure::Other(other),
    }
}

/// `(id, rev)` a block-level [`ReportDocOp`] resolved to: the created/
/// replaced/moved block's id and its post-op rev. `None` for the
/// wholesale and delete variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockOpOutcome {
    pub id: String,
    pub rev: u32,
}

/// The one place the "block not found" `BadRequest` is minted, paired
/// with [`is_block_not_found`] so the Batch arm can reclassify exactly
/// this error (and no other `BadRequest`) as proposal staleness.
pub(crate) fn block_not_found(id: &str) -> CalmError {
    CalmError::BadRequest(format!("block {id} not found"))
}

/// True iff `msg` came from [`block_not_found`]. Kept next to the
/// constructor; `batch_not_found_classifier_matches_constructor` pins
/// the pair against drift.
fn is_block_not_found(msg: &str) -> bool {
    msg.starts_with("block ") && msg.ends_with(" not found")
}

/// Execute `op` against the (already migrated) doc. Returns
/// `CalmError::Conflict` on an `if_rev` mismatch — the persist closure
/// propagates it, aborting the transaction, so a conflicting op writes
/// nothing and emits nothing. Unknown ids / out-of-range indexes are
/// `CalmError::BadRequest`.
pub(crate) fn apply_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
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
    match op {
        ReportDocOp::Replace { summary, body } => {
            let summary = tx_summary(doc, summary)?;
            validate_body_fences(body)?;
            guard_non_prose_stomp(doc, body)?;
            doc.update(&summary, body).map_err(internal)?;
            Ok(None)
        }
        ReportDocOp::WriteMarkdown { summary, body } => {
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
            if_rev,
        } => {
            let current = doc
                .block_rev(id)
                .map_err(|e| CalmError::Internal(format!("wave_report: block rev: {e}")))?
                .ok_or_else(|| block_not_found(id))?;
            if let Some(expected) = if_rev {
                check_rev(doc, id, *expected)?;
            }
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
        ReportDocOp::Batch(ops) => {
            check_batch_envelope(ops.len())?;
            for member in ops {
                check_batch_member_kind(member)?;
            }
            // Whole-batch atomicity at the doc level: run every op on
            // a scratch copy and only swap it in on full success. The
            // persist tx would roll back the columns anyway, but the
            // caller-visible `doc` must also stay untouched so a
            // failed batch cannot leak partial state into any
            // subsequent in-tx read.
            let mut scratch = fork_doc(doc)?;
            for op in ops {
                apply_report_op(&mut scratch, op).map_err(classify_batch_member_error)?;
            }
            *doc = scratch;
            Ok(None)
        }
        // #955 §5.2.1 (PR-b) — same machinery as `Batch`, but each
        // member is lowered from the wire shape against the scratch doc
        // immediately before it runs (kernel-minted ids for `temp_id`
        // creations, anchors → indexes). Delegated so the lowering rules
        // live next to the wire DTO they interpret.
        ReportDocOp::ProposalBatch {
            base_doc_heads,
            ops,
        } => {
            crate::wave_report_proposal::apply_proposal_batch(doc, base_doc_heads, ops)?;
            Ok(None)
        }
    }
}

/// Max members one batch-shaped op may carry. Shared by
/// [`ReportDocOp::Batch`] and [`ReportDocOp::ProposalBatch`], and by
/// `neige.proposal.submit`'s §5.2 op quota — one number, so the submit
/// gate and the apply gate cannot disagree.
pub const MAX_BATCH_OPS: usize = 32;

/// The shape invariants EVERY batch-shaped op shares (§5.2.2): at least
/// one member, at most [`MAX_BATCH_OPS`].
///
/// Both arms of [`apply_report_op`] funnel through here rather than
/// re-implementing the check, which is what stops a future rule added
/// for `Batch` from silently missing `ProposalBatch`. Enforcing the
/// count at apply time (not only at submit) matters because a replayed
/// or legacy `ProposalSubmitted` event can carry ops the current submit
/// gate would have refused.
pub(crate) fn check_batch_envelope(len: usize) -> Result<(), CalmError> {
    if len == 0 {
        return Err(CalmError::BadRequest(
            "ops must contain at least one operation".into(),
        ));
    }
    if len > MAX_BATCH_OPS {
        return Err(CalmError::BadRequest(format!(
            "{len} ops exceeds the {MAX_BATCH_OPS}-op batch limit"
        )));
    }
    Ok(())
}

/// Member kinds no batch may contain, for either batch flavour.
///
/// Nested batches would break the flat "one scratch fork, one pass"
/// atomicity model. The wholesale writes are denied at this kernel choke
/// point so an upstream op-mapping bug can never turn a proposal into a
/// full-body rewrite (§5.2.2) — `ProposalOp` cannot express any of them
/// today, and routing the proposal path through the same function is
/// what keeps that a checked fact rather than a hope.
pub(crate) fn check_batch_member_kind(member: &ReportDocOp) -> Result<(), CalmError> {
    match member {
        ReportDocOp::Batch(_) | ReportDocOp::ProposalBatch { .. } => Err(CalmError::BadRequest(
            "nested batches are not allowed — flatten the op list".into(),
        )),
        ReportDocOp::Replace { .. } | ReportDocOp::WriteMarkdown { .. } => {
            Err(CalmError::BadRequest(
                "replace/write_markdown are not allowed inside a batch — proposals carry \
                 block-level ops only"
                    .into(),
            ))
        }
        ReportDocOp::UpsertBlock { .. }
        | ReportDocOp::MoveBlock { .. }
        | ReportDocOp::DeleteBlock { .. } => Ok(()),
    }
}

/// Copy a doc so a failing batch cannot leak partial state into the
/// caller-visible document (the persist tx would roll the columns back
/// regardless, but subsequent in-tx reads must see the pre-batch state).
pub(crate) fn fork_doc(doc: &mut ReportDoc) -> Result<ReportDoc, CalmError> {
    ReportDoc::from_bytes(&doc.to_bytes())
        .map_err(|e| CalmError::Internal(format!("wave_report: fork doc: {e}")))
}

/// §5.2.1 staleness vs malformation, applied to one batch member's
/// error. Rev mismatches (already `Conflict`) and unknown block ids
/// ("the block you anchored on is gone" — snapshot decay, fixable by
/// re-reading) map to the proposal-stale `Conflict` class. Every other
/// `BadRequest` is structural (missing `if_rev`, out-of-range index,
/// unresolvable `temp:` reference): no fresh snapshot can fix it, so it
/// must stay `BadRequest` — mapping it to `Conflict` would invite
/// endless resubmission of an unfixable proposal. `Internal`
/// (corruption) passes through unmapped.
pub(crate) fn classify_batch_member_error(e: CalmError) -> CalmError {
    match e {
        CalmError::Conflict(msg) => CalmError::Conflict(format!("{PROPOSAL_BATCH_CONFLICT}{msg}")),
        CalmError::BadRequest(msg) if is_block_not_found(&msg) => {
            CalmError::Conflict(format!("{PROPOSAL_BATCH_CONFLICT}{msg}"))
        }
        CalmError::BadRequest(msg) => {
            CalmError::BadRequest(format!("proposal batch invalid: {msg}"))
        }
        other => other,
    }
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
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
) -> Result<Card, CalmError> {
    let outcome = persist_report_with_shadow(
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
        },
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
        None,
    )
    .await?;
    let (updated, _block) = outcome.into_written()?;
    Ok(updated)
}

/// Issue #955 §5.6 (PR-b) — the proposal-accept writer: resolve the
/// pending proposal and apply its lowered ops in ONE transaction.
///
/// Wraps [`persist_report_with_shadow`] with the two accept-specific
/// bindings the design pins: the events carry `actor = Kernel` for the
/// report change (the kernel writes on the plugin's behalf; plugin
/// attribution rides on `author`/`author_plugin_id`, §5.3) while the
/// `ProposalResolved` event inside the same tx carries `actor = User`.
///
/// Both verdicts commit: `accepted` with the report change,
/// [`ProposalAcceptOutcome::Stale`] with no report change at all. Only a
/// lost double-resolve race (409) or a structural/internal failure comes
/// back as an `Err` — see [`classify_accept_error`].
#[allow(clippy::too_many_arguments)]
pub async fn persist_report_proposal_accept(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    base_doc_heads: String,
    ops: Vec<calm_types::proposal::ProposalOp>,
    accept: ProposalAcceptCtx,
) -> Result<ProposalAcceptOutcome, AcceptFailure> {
    persist_report_with_shadow(
        repo,
        events,
        write,
        ActorId::Kernel,
        EditAuthor::Plugin,
        wave,
        report_card,
        current_payload,
        ReportDocOp::ProposalBatch {
            base_doc_heads,
            ops,
        },
        None,
        None,
        false,
        None,
        Some(accept),
    )
    .await
    .map(|outcome| match outcome {
        PersistOutcome::Written { card, .. } => ProposalAcceptOutcome::Accepted(card),
        PersistOutcome::ProposalStale { reason } => ProposalAcceptOutcome::Stale(reason),
    })
    .map_err(classify_accept_error)
}

/// How a [`persist_report_with_shadow`] transaction ended.
///
/// Every caller but the #955 accept path only ever sees `Written` (the
/// stale arm needs a `ProposalAcceptCtx` to exist at all), so they use
/// [`PersistOutcome::into_written`] to flatten it back.
// Same one-shot rationale as `ProposalAcceptOutcome`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum PersistOutcome {
    /// The op applied: the card row + CRDT blob were rewritten and the
    /// `CardUpdated` / `WaveReportEdited` pair emitted.
    Written {
        card: Card,
        block: Option<BlockOpOutcome>,
    },
    /// #955 §5.6, accept path only: the proposal batch hit an anchoring
    /// conflict, so this transaction recorded `ProposalResolved { stale }`
    /// and committed WITHOUT touching the report.
    ProposalStale { reason: String },
}

impl PersistOutcome {
    /// Flatten the outcome for the non-proposal callers. `ProposalStale`
    /// is unreachable without a `ProposalAcceptCtx`, so seeing it here is
    /// a kernel bug, not a runtime condition — hence `Internal`.
    pub(crate) fn into_written(self) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
        match self {
            PersistOutcome::Written { card, block } => Ok((card, block)),
            PersistOutcome::ProposalStale { reason } => Err(CalmError::Internal(format!(
                "wave_report: non-proposal write reported a proposal-stale outcome: {reason}"
            ))),
        }
    }
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
    // `Some` ⇒ this is a #955 §5.6 proposal accept: re-check + resolve
    // the proposal in the same tx as the apply. See `ProposalAcceptCtx`.
    proposal: Option<ProposalAcceptCtx>,
) -> Result<PersistOutcome, CalmError> {
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
    let (updated, _ids) =
        write_with_actor_events_typed::<PersistOutcome, _>(repo, None, events, write, move |tx| {
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
            let proposal = proposal.clone();
            Box::pin(async move {
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                // #955 §5.6 — the accept path's authoritative pending
                // re-check, inside the write tx. SQLite's single writer
                // makes "first commit wins": a concurrent
                // accept/reject/withdraw either already terminalized the
                // row (we see it and bail) or serializes behind us.
                // The wave/plugin binds mirror the projection's
                // choke-point backstop so a route bug can never resolve
                // one plugin's proposal under another's identity.
                if let Some(accept) = proposal.as_ref() {
                    let row = crate::db::sqlite::proposal_get_tx(tx, &accept.proposal_id)
                        .await?
                        .ok_or_else(|| CalmError::Conflict(PROPOSAL_NOT_PENDING.into()))?;
                    if row.status != "pending"
                        || row.plugin_id != accept.plugin_id
                        || row.wave_id != wave_id.as_str()
                    {
                        return Err(CalmError::Conflict(PROPOSAL_NOT_PENDING.into()));
                    }
                }
                // The resolve event is NOT pushed yet: which decision it
                // carries is only known once the ops have been tried, and
                // §5.6 requires the verdict to commit in the very tx that
                // determined it. See the `apply_report_op` call below.
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
                //
                //    The one exception is the #955 §5.6 accept path: an
                //    anchoring conflict there is not a failure, it IS the
                //    `stale` verdict, and the verdict is only
                //    authoritative in the transaction that determined it.
                //    So we record it HERE and commit, instead of rolling
                //    back and racing a second transaction against a
                //    concurrent reject/withdraw.
                let outcome = match apply_report_op(&mut doc, &op) {
                    Ok(outcome) => outcome,
                    Err(CalmError::Conflict(reason))
                        if proposal.is_some() && reason.starts_with(PROPOSAL_BATCH_CONFLICT) =>
                    {
                        let accept = proposal.as_ref().expect("checked by the guard");
                        events.push((
                            ActorId::User,
                            wave_scope.clone(),
                            Event::ProposalResolved {
                                wave_id: wave_id.clone(),
                                proposal_id: accept.proposal_id.clone(),
                                plugin_id: accept.plugin_id.clone(),
                                decision: calm_types::proposal::ProposalDecision::Stale,
                            },
                        ));
                        // Return BEFORE step 4/5: no `card_update_with_crdt_tx`,
                        // so neither the projected payload nor the CRDT
                        // blob is written. `apply_proposal_batch` runs on
                        // its own scratch fork and only swaps it in on
                        // full success, so `doc` here is byte-identical to
                        // what was loaded — the in-memory legacy-layout
                        // migration from step 1 is discarded along with
                        // it, exactly as it would have been by a rollback.
                        return Ok((PersistOutcome::ProposalStale { reason }, events));
                    }
                    Err(e) => return Err(e),
                };
                // The apply succeeded, so the accept path's verdict is
                // `accepted`; it goes in front of the report events so a
                // consumer reading the batch in order sees the decision
                // before its effect. Actor is the human adjudicator
                // (§5.4's User-only hard clause) even though the report
                // change below is written by the kernel — §5.3 accepts
                // the mixed-actor transaction explicitly.
                if let Some(accept) = proposal.as_ref() {
                    events.push((
                        ActorId::User,
                        wave_scope.clone(),
                        Event::ProposalResolved {
                            wave_id: wave_id.clone(),
                            proposal_id: accept.proposal_id.clone(),
                            plugin_id: accept.plugin_id.clone(),
                            decision: calm_types::proposal::ProposalDecision::Accepted,
                        },
                    ));
                }
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
                projected_payload.blocks = Some(doc.blocks_snapshot().map_err(|e| {
                    CalmError::Internal(format!(
                        "wave_report: snapshot CRDT blocks for card {id}: {e}"
                    ))
                })?);
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
                let report_edited = Event::WaveReportEdited {
                    wave_id,
                    card_id: report_card_id,
                    author,
                    // #955 §5.3 — the accept path (and only it) passes
                    // `author = Plugin` and threads the submitter id
                    // through here; Spec/User/Kernel edits leave the
                    // field None by contract.
                    author_plugin_id: proposal.as_ref().map(|p| p.plugin_id.clone()),
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
                events.push((actor, scope, report_edited));
                Ok((
                    PersistOutcome::Written {
                        card: updated,
                        block: outcome,
                    },
                    events,
                ))
            })
        })
        .await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
                "schemaVersion": 2,
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
            },
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
            },
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "racing summary");
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: Some("explicit".into()),
                body: "# C\n\ngamma\n".into(),
            },
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "explicit");
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
                position: None,
            },
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
            },
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");
    }

    // ----- #955 §5.2.2: ReportDocOp::Batch --------------------------------

    /// Two-block doc the batch tests operate on. Returns the doc plus
    /// its `(id, rev)` snapshot in order.
    fn two_block_doc() -> (ReportDoc, Vec<(String, u32)>) {
        let doc = ReportDoc::from_payload(&WaveReportPayload::new(
            "s",
            "# A\n\nalpha\n\n# B\n\nbeta\n",
        ));
        let blocks = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .map(|b| (b.id, b.rev))
            .collect::<Vec<_>>();
        assert_eq!(blocks.len(), 2, "seed body must split into two blocks");
        (doc, blocks)
    }

    #[test]
    fn batch_applies_ops_sequentially_from_one_snapshot() {
        let (mut doc, blocks) = two_block_doc();
        let (a_id, a_rev) = blocks[0].clone();
        // Create a new block, then delete an existing one — both must
        // land, in order, through a single op.
        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch(vec![
                ReportDocOp::UpsertBlock {
                    id: None,
                    kind: "prose".into(),
                    content: "# C\n\ngamma\n".into(),
                    if_rev: None,
                    position: None,
                },
                ReportDocOp::DeleteBlock {
                    id: a_id.clone(),
                    if_rev: a_rev,
                },
            ]),
        )
        .unwrap();
        assert!(outcome.is_none(), "batch reports no single-block outcome");
        let after = doc.blocks_snapshot().unwrap();
        assert_eq!(after.len(), 2, "one created + one deleted: {after:?}");
        assert!(after.iter().all(|b| b.id != a_id), "block A must be gone");
        let (_, body) = doc.project().unwrap();
        assert!(body.contains("gamma"), "created block must project: {body}");
        assert!(!body.contains("alpha"), "deleted block must not project");
    }

    #[test]
    fn batch_mid_failure_rolls_back_the_whole_batch() {
        let (mut doc, blocks) = two_block_doc();
        let (a_id, a_rev) = blocks[0].clone();
        let (b_id, b_rev) = blocks[1].clone();
        let before = doc.project().unwrap();

        // Op 1 would succeed; op 2 has a stale rev — the WHOLE batch
        // must roll back, leaving op 1's delete unapplied.
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch(vec![
                ReportDocOp::DeleteBlock {
                    id: a_id.clone(),
                    if_rev: a_rev,
                },
                ReportDocOp::DeleteBlock {
                    id: b_id.clone(),
                    if_rev: b_rev + 41,
                },
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(err, CalmError::Conflict(_)),
            "rev mismatch inside a batch is the conflict class: {err:?}"
        );
        assert_eq!(
            doc.project().unwrap(),
            before,
            "mid-batch failure must leave the doc untouched"
        );
        assert_eq!(doc.blocks_snapshot().unwrap().len(), 2);
    }

    #[test]
    fn batch_unknown_block_id_is_conflict_not_bad_request() {
        // §5.2.1: for an async proposal, an unknown block id means the
        // anchor vanished under it — staleness, not a malformed
        // request. (Outside a batch the same op stays BadRequest.)
        let (mut doc, _) = two_block_doc();
        let op = ReportDocOp::DeleteBlock {
            id: "b_gone".into(),
            if_rev: 1,
        };
        let solo = apply_report_op(&mut doc, &op.clone()).unwrap_err();
        assert!(matches!(solo, CalmError::BadRequest(_)), "solo: {solo:?}");
        let batched = apply_report_op(&mut doc, &ReportDocOp::Batch(vec![op])).unwrap_err();
        assert!(
            matches!(batched, CalmError::Conflict(_)),
            "batched: {batched:?}"
        );
    }

    #[test]
    fn batch_can_reference_a_block_created_earlier_in_the_batch() {
        // Sequential validate-and-apply: op 2 replaces content the doc
        // only holds because op 1 ran first. (Positional temp-id
        // resolution is the PR-b submit layer's job — here we pin the
        // underlying sequencing semantics.)
        let (mut doc, blocks) = two_block_doc();
        let (a_id, a_rev) = blocks[0].clone();
        // delete-then-move the SAME block: contradictory sequence must
        // fail as a whole.
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch(vec![
                ReportDocOp::DeleteBlock {
                    id: a_id.clone(),
                    if_rev: a_rev,
                },
                ReportDocOp::MoveBlock {
                    id: a_id.clone(),
                    to_index: 0,
                    if_rev: Some(a_rev),
                },
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Conflict(_)), "got {err:?}");
        assert_eq!(doc.blocks_snapshot().unwrap().len(), 2, "rolled back");
    }

    #[test]
    fn batch_rejects_empty_and_nested_shapes() {
        let (mut doc, blocks) = two_block_doc();
        let err = apply_report_op(&mut doc, &ReportDocOp::Batch(vec![])).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)), "empty: {err:?}");
        let (a_id, a_rev) = blocks[0].clone();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Batch(vec![ReportDocOp::Batch(vec![ReportDocOp::DeleteBlock {
                id: a_id,
                if_rev: a_rev,
            }])]),
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)), "nested: {err:?}");
    }

    #[test]
    fn batch_rejects_wholesale_members_as_bad_request() {
        // §5.2.2 — the proposal wire cannot express Replace /
        // WriteMarkdown; the kernel entry must deny them too so an
        // op-mapping bug can never smuggle a full-body rewrite in.
        let (mut doc, _) = two_block_doc();
        let before = doc.project().unwrap();
        for member in [
            ReportDocOp::Replace {
                summary: None,
                body: "# X\n\nstomp\n".into(),
            },
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: "# X\n\nstomp\n".into(),
            },
        ] {
            let err =
                apply_report_op(&mut doc, &ReportDocOp::Batch(vec![member.clone()])).unwrap_err();
            assert!(
                matches!(err, CalmError::BadRequest(_)),
                "wholesale member {member:?} must be BadRequest: {err:?}"
            );
        }
        assert_eq!(doc.project().unwrap(), before, "doc untouched");
    }

    #[test]
    fn batch_structural_errors_stay_bad_request_not_conflict() {
        // A structural malformation (missing if_rev, out-of-range
        // index) can never succeed against ANY snapshot — mapping it
        // to Conflict would tell the plugin to resubmit forever.
        let (mut doc, blocks) = two_block_doc();
        let (a_id, _) = blocks[0].clone();
        let missing_if_rev = ReportDocOp::UpsertBlock {
            id: Some(a_id.clone()),
            kind: "prose".into(),
            content: "# A\n\nnew\n".into(),
            if_rev: None,
            position: None,
        };
        let out_of_range = ReportDocOp::MoveBlock {
            id: a_id,
            to_index: 99,
            if_rev: None,
        };
        for op in [missing_if_rev, out_of_range] {
            let err = apply_report_op(&mut doc, &ReportDocOp::Batch(vec![op.clone()])).unwrap_err();
            assert!(
                matches!(err, CalmError::BadRequest(_)),
                "structural member {op:?} must stay BadRequest: {err:?}"
            );
        }
        assert_eq!(doc.blocks_snapshot().unwrap().len(), 2, "doc untouched");
    }

    /// #955 §5.6 — the accept path's two conflict classes must never be
    /// confused: a lost double-resolve race is a 409, a failed anchor is
    /// the `stale` decision. Both arrive as `CalmError::Conflict`, so the
    /// sentinel/prefix pair below is load-bearing.
    #[test]
    fn accept_error_classes_are_disjoint() {
        assert!(matches!(
            classify_accept_error(CalmError::Conflict(PROPOSAL_NOT_PENDING.into())),
            AcceptFailure::NotPending
        ));
        // Anything the batch classifier mints carries the prefix — which
        // is the fact the IN-TX stale arm keys on (it can only resolve
        // `stale` for a conflict it is certain came from the batch, never
        // for an arbitrary Conflict escaping some other in-tx read).
        let batched =
            classify_batch_member_error(CalmError::Conflict("rev conflict on block b_0001".into()));
        assert!(
            matches!(&batched, CalmError::Conflict(msg) if msg.starts_with(PROPOSAL_BATCH_CONFLICT)),
            "got {batched:?}"
        );
        let vanished = classify_batch_member_error(block_not_found("b_0001"));
        assert!(
            matches!(&vanished, CalmError::Conflict(msg) if msg.starts_with(PROPOSAL_BATCH_CONFLICT)),
            "got {vanished:?}"
        );
        // Should one ever escape the transaction unrecognized, it must NOT
        // be mistaken for a double-resolve race — it is a plain error, and
        // the proposal stays pending rather than silently terminalizing.
        assert!(matches!(
            classify_accept_error(batched),
            AcceptFailure::Other(CalmError::Conflict(_))
        ));
        // Structural malformation stays an error — never silently stale.
        let structural = classify_batch_member_error(CalmError::BadRequest(
            "if_rev is required when replacing an existing block".into(),
        ));
        assert!(matches!(
            classify_accept_error(structural),
            AcceptFailure::Other(CalmError::BadRequest(_))
        ));
        // And the two sentinels cannot alias each other.
        assert!(!PROPOSAL_NOT_PENDING.starts_with(PROPOSAL_BATCH_CONFLICT));
        assert!(!PROPOSAL_BATCH_CONFLICT.starts_with(PROPOSAL_NOT_PENDING));
    }

    #[test]
    fn batch_not_found_classifier_matches_constructor() {
        // Pins the constructor/classifier pair the Batch reclassifier
        // relies on, including an id that itself contains spaces.
        for id in ["b_0001", "weird id with spaces"] {
            let CalmError::BadRequest(msg) = block_not_found(id) else {
                panic!("block_not_found must be BadRequest");
            };
            assert!(is_block_not_found(&msg), "constructor output: {msg}");
        }
        assert!(!is_block_not_found(
            "if_rev is required when replacing an existing block"
        ));
        assert!(!is_block_not_found(
            "to_index 99 out of range (report has 2 blocks)"
        ));
    }

    /// Persist-level pin for §5.2.2: one successful Batch ⇒ exactly ONE
    /// `CardUpdated` + ONE `WaveReportEdited` pair; a conflicting Batch
    /// ⇒ zero events, untouched row.
    #[tokio::test]
    async fn persist_batch_emits_exactly_one_event_pair() {
        use crate::card_role_cache::CardRoleCache;
        use crate::db::prelude::*;
        use crate::db::sqlite::SqlxRepo;
        use crate::model::{NewCard, NewCove, NewWave};
        use crate::wave_cove_cache::WaveCoveCache;
        use std::sync::Arc;

        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "batch".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id.clone(),
                title: "batch wave".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let _report_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                title: None,
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())
                    .expect("initial report payload"),
            })
            .await
            .expect("create report card");
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_wave_cove_cache(&wave_cove_cache)
            .await
            .expect("seed wave cove cache");
        let write = crate::state::WriteContext::new(CardRoleCache::new(), wave_cove_cache);
        let events = EventBus::new();
        let route_repo: &dyn RouteRepo = repo.as_ref();

        // Establish the CRDT (initial body → one prose block) so the
        // batch below has a real block id + rev to anchor on.
        let (wave_row, card_row, payload) = resolve_report_for_wave(route_repo, wave.id.as_str())
            .await
            .expect("resolve report");
        let (updated, _) = persist_report_with_shadow(
            route_repo,
            &events,
            &write,
            ActorId::User,
            EditAuthor::User,
            wave_row,
            card_row,
            payload,
            ReportDocOp::Replace {
                summary: Some("seeded".into()),
                body: "# A\n\nalpha\n".into(),
            },
            None,
            None,
            false,
            None,
            None,
        )
        .await
        .expect("seed persist")
        .into_written()
        .expect("seed wrote the report");
        let seeded: WaveReportPayload =
            serde_json::from_value(updated.payload.clone()).expect("seeded payload");
        let block = seeded.blocks.as_ref().expect("blocks")[0].clone();

        let baseline = repo
            .events_since(0, i64::MAX)
            .await
            .expect("events baseline")
            .len();

        let (wave_row, card_row, payload) = resolve_report_for_wave(route_repo, wave.id.as_str())
            .await
            .expect("re-resolve report");
        persist_report_with_shadow(
            route_repo,
            &events,
            &write,
            ActorId::User,
            EditAuthor::User,
            wave_row.clone(),
            card_row.clone(),
            payload.clone(),
            ReportDocOp::Batch(vec![
                ReportDocOp::UpsertBlock {
                    id: None,
                    kind: "prose".into(),
                    content: "# B\n\nbeta\n".into(),
                    if_rev: None,
                    position: None,
                },
                ReportDocOp::UpsertBlock {
                    id: Some(block.id.clone()),
                    kind: "prose".into(),
                    content: "# A\n\nalpha v2\n".into(),
                    if_rev: Some(block.rev),
                    position: None,
                },
            ]),
            None,
            None,
            false,
            None,
            None,
        )
        .await
        .expect("batch persist");

        let all = repo.events_since(0, i64::MAX).await.expect("events after");
        let new: Vec<_> = all[baseline..].iter().map(|(_, _, _, ev)| ev).collect();
        assert_eq!(
            new.len(),
            2,
            "one Batch ⇒ exactly one event pair, got {new:?}"
        );
        assert!(matches!(new[0], Event::CardUpdated(_)), "got {new:?}");
        assert!(
            matches!(new[1], Event::WaveReportEdited { .. }),
            "got {new:?}"
        );

        // A conflicting batch aborts the tx: no events, row untouched.
        let baseline = all.len();
        let (wave_row, card_row, payload) = resolve_report_for_wave(route_repo, wave.id.as_str())
            .await
            .expect("re-resolve report again");
        let before_payload = payload.clone();
        let err = persist_report_with_shadow(
            route_repo,
            &events,
            &write,
            ActorId::User,
            EditAuthor::User,
            wave_row,
            card_row,
            payload,
            ReportDocOp::Batch(vec![ReportDocOp::DeleteBlock {
                id: block.id.clone(),
                if_rev: block.rev + 41,
            }]),
            None,
            None,
            false,
            None,
            None,
        )
        .await
        .expect_err("stale batch must conflict");
        assert!(matches!(err, CalmError::Conflict(_)), "got {err:?}");
        let all = repo.events_since(0, i64::MAX).await.expect("events final");
        assert_eq!(all.len(), baseline, "failed batch must emit nothing");
        let (_, _, payload_after) = resolve_report_for_wave(route_repo, wave.id.as_str())
            .await
            .expect("resolve after failed batch");
        assert_eq!(payload_after, before_payload, "row must be untouched");
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
