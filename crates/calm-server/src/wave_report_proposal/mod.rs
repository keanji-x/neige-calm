//! Issue #955 §5.2.1 (PR-b) — lowering the ④ proposal channel's wire
//! ops onto the report document.
//!
//! `ProposalOp` (`calm_types::proposal`) is deliberately *not* the
//! kernel's internal op shape: it anchors positions on stable block ids
//! (`anchor: after_block_id | at_start | at_end`) and names blocks it
//! creates with a proposal-local `temp_id`, because a proposal is written
//! asynchronously and numeric indexes would silently drift under an
//! unrelated insertion. `ReportDocOp`, by contrast, speaks list indexes
//! and durable `b_xxxx` ids.
//!
//! Translating between them is only possible **inside** the persist
//! transaction, one op at a time:
//!
//!   * a `temp_id` creation's durable id is minted by the kernel when the
//!     op actually runs (§5.2.1: "持久 id 由内核在 apply 时分配"), and
//!     later ops may anchor on it via `temp:<temp_id>`;
//!   * an anchor's index depends on the document *as the preceding ops
//!     left it* (sequential validate-and-apply).
//!
//! So [`apply_proposal_batch`] interleaves lowering and application on a
//! scratch document, and [`validate_proposal_ops`] runs the purely
//! structural half of the same rules at submit time so a malformed
//! proposal is refused at the door instead of rotting until someone
//! presses accept.

use std::collections::{HashMap, HashSet};

use calm_types::proposal::{ProposalAnchor, ProposalOp};
use calm_types::report_blocks;

use crate::error::CalmError;
use crate::wave_report::{
    ReportDocOp, apply_report_op, block_not_found, check_batch_envelope, check_batch_member_kind,
    classify_batch_member_error, fork_doc,
};
use crate::wave_report_doc::ReportDoc;

/// Prefix marking an anchor reference to a block created earlier in the
/// same proposal (§5.2.1). Durable block ids use the `b_xxxx` shape, so
/// the namespaces cannot collide.
pub const TEMP_REF_PREFIX: &str = "temp:";

// ---------------------------------------------------------------------------
// Submit-time structural validation
// ---------------------------------------------------------------------------

/// Structural rules from §5.2.1, checkable without a document. Returns
/// the human message for the first violation (the submit handler turns
/// it into `InvalidParams`).
///
/// Deliberately **not** checked here: anything that needs the live doc
/// (`if_rev` values, existence of `block_id`/`after_block_id` targets,
/// `base_doc_heads`). Those are the accept transaction's authoritative
/// anchoring checks, and pre-judging them at submit time would reject
/// proposals that are perfectly acceptable by the time a human looks at
/// them — the opposite of what an async channel needs.
pub fn validate_proposal_ops(ops: &[ProposalOp]) -> Result<(), String> {
    if ops.is_empty() {
        return Err("ops must contain at least one operation".into());
    }
    // Temp ids seen so far — a `temp:` reference may only point BACKWARD
    // (the referenced block must already exist when the anchor resolves).
    let mut declared: Vec<&str> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let at = |msg: String| format!("ops[{i}]: {msg}");
        match op {
            ProposalOp::UpsertBlock {
                block_id,
                temp_id,
                kind,
                payload,
                if_rev,
                anchor,
            } => {
                match (block_id.as_deref(), temp_id.as_deref()) {
                    (Some(_), Some(_)) => {
                        return Err(at("exactly one of `block_id` (replace an existing block) \
                                        or `temp_id` (create a new one) may be set, not both"
                            .into()));
                    }
                    (None, None) => {
                        return Err(at("upsert_block needs either `block_id` (replace) or \
                                        `temp_id` (create)"
                            .into()));
                    }
                    (Some(_), None) => {
                        // Replace: `if_rev` is mandatory (async anchoring
                        // must be complete — unlike the interactive
                        // `calm.report.blocks.upsert` tool), and there is
                        // nothing to position (use move_block to reorder).
                        if if_rev.is_none() {
                            return Err(at(
                                "`if_rev` is required when `block_id` is given (read it \
                                 from neige.report.get)"
                                    .into(),
                            ));
                        }
                        if anchor.is_some() {
                            return Err(at(
                                "`anchor` is only valid when creating a block (`temp_id`) \
                                 — propose a move_block op to reorder an existing one"
                                    .into(),
                            ));
                        }
                    }
                    (None, Some(temp_id)) => {
                        // NOTE the ordering below: the anchor is validated
                        // against `declared` BEFORE this op's own `temp_id`
                        // joins it. §5.2.1 allows a `temp:` reference to a
                        // *previously* created block only, so
                        // `{temp_id: "t", anchor: {after_block_id: "temp:t"}}`
                        // — a creation anchored after itself — must be
                        // refused here at submit, not survive to accept as
                        // an unfixable structural 400.
                        if temp_id.is_empty() {
                            return Err(at("`temp_id` must be non-empty".into()));
                        }
                        if temp_id.starts_with(TEMP_REF_PREFIX) {
                            return Err(at(format!(
                                "`temp_id` must not include the `{TEMP_REF_PREFIX}` prefix \
                                 — that form is only used inside `anchor.after_block_id`"
                            )));
                        }
                        if declared.contains(&temp_id) {
                            return Err(at(format!(
                                "duplicate `temp_id` `{temp_id}` — temp ids must be unique \
                                 within a proposal"
                            )));
                        }
                        if if_rev.is_some() {
                            return Err(at(
                                "`if_rev` is meaningless when creating a block — omit it".into(),
                            ));
                        }
                        let Some(anchor) = anchor else {
                            return Err(at(
                                "`anchor` is required when creating a block (at_start, \
                                 at_end, or { after_block_id })"
                                    .into(),
                            ));
                        };
                        check_anchor_refs(anchor, &declared).map_err(&at)?;
                        declared.push(temp_id);
                    }
                }
                // Payload → stored content must be renderable NOW: a bad
                // block kind or schema-invalid payload is structural, so
                // it can never become acceptable later.
                render_block_content(kind, payload).map_err(&at)?;
            }
            ProposalOp::MoveBlock {
                block_id, anchor, ..
            } => {
                if block_id.is_empty() {
                    return Err(at("`block_id` must be non-empty".into()));
                }
                check_anchor_refs(anchor, &declared).map_err(&at)?;
                if let ProposalAnchor::AfterBlockId(target) = anchor
                    && target == block_id
                {
                    return Err(at(
                        "a block cannot be anchored after itself — drop the op".into()
                    ));
                }
            }
            ProposalOp::DeleteBlock { block_id, .. } => {
                if block_id.is_empty() {
                    return Err(at("`block_id` must be non-empty".into()));
                }
            }
        }
    }
    Ok(())
}

/// A `temp:<id>` anchor may only reference a temp id declared by an
/// EARLIER op in the same proposal. Plain ids are left to the accept
/// transaction (their existence is a staleness question, not a
/// structural one).
fn check_anchor_refs(anchor: &ProposalAnchor, declared: &[&str]) -> Result<(), String> {
    let ProposalAnchor::AfterBlockId(target) = anchor else {
        return Ok(());
    };
    if target.is_empty() {
        return Err("`anchor.after_block_id` must be non-empty".into());
    }
    if let Some(temp) = target.strip_prefix(TEMP_REF_PREFIX)
        && !declared.contains(&temp)
    {
        return Err(format!(
            "`anchor.after_block_id` references `{target}`, but no earlier op in this \
             proposal creates a block with that temp_id"
        ));
    }
    Ok(())
}

/// Render a proposed block's `(kind, payload)` into the flat text the
/// document stores.
///
/// The content *rules* are not restated here: they live in
/// `calm_types::report_blocks` ([`check_prose_markdown`],
/// [`render_data_block`], [`unknown_kind_message`]) and are shared with
/// `calm.report.blocks.upsert`, so a proposal cannot express a block
/// shape the interactive tool would have refused — and a new data kind
/// or fence rule cannot land on one surface only. What stays here is the
/// envelope: this wire shape always carries the markdown inside
/// `payload.markdown` (the tool also accepts it top-level).
///
/// [`check_prose_markdown`]: calm_types::report_blocks::check_prose_markdown
/// [`render_data_block`]: calm_types::report_blocks::render_data_block
/// [`unknown_kind_message`]: calm_types::report_blocks::unknown_kind_message
pub fn render_block_content(kind: &str, payload: &serde_json::Value) -> Result<String, String> {
    if kind == report_blocks::KIND_PROSE {
        let markdown = match payload.get("markdown") {
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => {
                return Err("kind=prose requires `payload.markdown` (string)".to_string());
            }
        };
        report_blocks::check_prose_markdown(&markdown)?;
        Ok(markdown)
    } else if report_blocks::is_data_kind(kind) {
        if !payload.is_object() {
            return Err(format!("kind={kind} requires a `payload` object"));
        }
        report_blocks::render_data_block(kind, payload)
    } else {
        Err(report_blocks::unknown_kind_message(kind))
    }
}

// ---------------------------------------------------------------------------
// Apply time
// ---------------------------------------------------------------------------

/// Sequentially lower-and-apply a whole proposal onto `doc` (§5.2.1
/// "顺序校验-应用", §5.2.2 atomicity).
///
/// Runs on a scratch fork that only replaces `doc` on full success, so a
/// failure leaves the caller-visible document untouched. Errors come back
/// already classified by
/// [`classify_batch_member_error`](crate::wave_report::classify_batch_member_error):
/// anchoring decay is the `Conflict` (stale) class, structural
/// malformation stays `BadRequest`.
pub(crate) fn apply_proposal_batch(
    doc: &mut ReportDoc,
    base_doc_heads: &str,
    ops: &[ProposalOp],
) -> Result<(), CalmError> {
    // Shared with `ReportDocOp::Batch` (one definition, two arms): a
    // batch is non-empty and carries at most `MAX_BATCH_OPS` members.
    // The op-count ceiling matters at APPLY time, not only at submit —
    // a replayed or legacy `ProposalSubmitted` event carrying more ops
    // than the submit gate would accept today must not apply.
    check_batch_envelope(ops.len()).map_err(|e| match e {
        CalmError::BadRequest(msg) => {
            CalmError::BadRequest(format!("proposal batch invalid: {msg}"))
        }
        other => other,
    })?;
    // §5.2.1: the whole-document anchor is authoritative only for
    // creations with no block-level anchor. Checked once, up front, on
    // the incoming doc — every op in the batch sees the same baseline.
    // NOTE the gate is `any(..)`: a proposal that MIXES a block-anchored
    // op with one at_start/at_end creation is heads-gated as a whole.
    if ops.iter().any(needs_whole_doc_anchor) {
        let current = doc.doc_heads();
        if current != base_doc_heads {
            return Err(CalmError::Conflict(format!(
                "{}the report changed since this proposal's baseline (base_doc_heads \
                 mismatch) and it creates a block anchored only at the document \
                 start/end — re-read the report and re-propose",
                crate::wave_report::PROPOSAL_BATCH_CONFLICT,
            )));
        }
    }
    let mut scratch = fork_doc(doc)?;
    // temp_id → the durable block id the kernel minted for it.
    let mut minted: HashMap<String, String> = HashMap::new();
    // Blocks an EARLIER op of this same batch deleted. Referencing one
    // is self-contradictory, not stale — see `ensure_not_self_deleted`.
    let mut deleted: HashSet<String> = HashSet::new();
    for (i, op) in ops.iter().enumerate() {
        let lowered = lower_proposal_op(&scratch, op, &minted, &deleted)
            .map_err(|e| prefix_member_error(i, classify_batch_member_error(e)))?;
        // Same member-kind ceiling the `Batch` arm applies, on the
        // LOWERED member — `ProposalOp` cannot express a nested batch or
        // a wholesale write today, and funnelling through the shared
        // check is what keeps that true if `Batch` grows a new rule.
        check_batch_member_kind(&lowered)
            .map_err(|e| prefix_member_error(i, classify_batch_member_error(e)))?;
        let outcome = apply_report_op(&mut scratch, &lowered)
            .map_err(|e| prefix_member_error(i, classify_batch_member_error(e)))?;
        match op {
            // Record the kernel-assigned id so later `temp:` anchors
            // resolve.
            ProposalOp::UpsertBlock {
                temp_id: Some(temp_id),
                ..
            } => {
                let outcome = outcome.ok_or_else(|| {
                    CalmError::Internal(format!(
                        "wave_report_proposal: ops[{i}] created a block but reported no id"
                    ))
                })?;
                minted.insert(temp_id.clone(), outcome.id);
            }
            ProposalOp::DeleteBlock { block_id, .. } => {
                deleted.insert(block_id.clone());
            }
            _ => {}
        }
    }
    *doc = scratch;
    Ok(())
}

/// True for a creation whose only positional information is
/// "document start" / "document end" — the ops `base_doc_heads` is
/// authoritative for (§5.2.1).
fn needs_whole_doc_anchor(op: &ProposalOp) -> bool {
    matches!(
        op,
        ProposalOp::UpsertBlock {
            block_id: None,
            anchor: Some(ProposalAnchor::AtStart | ProposalAnchor::AtEnd),
            ..
        }
    )
}

/// Tag a member failure with its op index without changing its class —
/// a proposal with 32 ops is unreadable otherwise.
fn prefix_member_error(i: usize, e: CalmError) -> CalmError {
    match e {
        CalmError::Conflict(msg) => CalmError::Conflict(format!("{msg} (at ops[{i}])")),
        CalmError::BadRequest(msg) => CalmError::BadRequest(format!("{msg} (at ops[{i}])")),
        other => other,
    }
}

/// Lower ONE wire op against the current scratch-document state.
///
/// Errors are raw (unclassified): `block_not_found` for a vanished
/// anchor target (the caller's classifier turns it into staleness), plain
/// `BadRequest` for structural problems.
fn lower_proposal_op(
    doc: &ReportDoc,
    op: &ProposalOp,
    minted: &HashMap<String, String>,
    deleted: &HashSet<String>,
) -> Result<ReportDocOp, CalmError> {
    let order = doc
        .block_index()
        .map_err(|e| CalmError::Internal(format!("wave_report_proposal: block index: {e}")))?;
    let ids: Vec<&str> = order.iter().map(|(id, _, _)| id.as_str()).collect();
    match op {
        ProposalOp::UpsertBlock {
            block_id,
            temp_id,
            kind,
            payload,
            if_rev,
            anchor,
        } => {
            let content = render_block_content(kind, payload).map_err(CalmError::BadRequest)?;
            match (block_id, temp_id) {
                (Some(block_id), None) => {
                    ensure_not_self_deleted(block_id, deleted)?;
                    // Replace. `if_rev` presence was already required at
                    // submit time; re-assert it here because apply is
                    // the authoritative layer (a stored proposal from a
                    // future/older shape must not slip through).
                    let if_rev = (*if_rev).ok_or_else(|| {
                        CalmError::BadRequest(
                            "if_rev is required when replacing an existing block".into(),
                        )
                    })?;
                    Ok(ReportDocOp::UpsertBlock {
                        id: Some(block_id.clone()),
                        kind: kind.clone(),
                        content,
                        if_rev: Some(if_rev),
                        position: None,
                    })
                }
                (None, Some(_)) => {
                    let anchor = anchor.as_ref().ok_or_else(|| {
                        CalmError::BadRequest("anchor is required when creating a block".into())
                    })?;
                    // `position` is the new block's FINAL index among
                    // len+1 blocks: 0 for at_start, len for at_end,
                    // (index of the anchor) + 1 for after_block_id.
                    let position = match anchor {
                        ProposalAnchor::AtStart => 0,
                        ProposalAnchor::AtEnd => ids.len(),
                        ProposalAnchor::AfterBlockId(target) => {
                            resolve_anchor_index(target, &ids, minted, deleted)? + 1
                        }
                    };
                    Ok(ReportDocOp::UpsertBlock {
                        id: None,
                        kind: kind.clone(),
                        content,
                        if_rev: None,
                        position: Some(position),
                    })
                }
                // Both / neither: rejected at submit; re-asserted here.
                _ => Err(CalmError::BadRequest(
                    "upsert_block needs exactly one of `block_id` or `temp_id`".into(),
                )),
            }
        }
        ProposalOp::MoveBlock {
            block_id,
            if_rev,
            anchor,
        } => {
            ensure_not_self_deleted(block_id, deleted)?;
            let from = ids
                .iter()
                .position(|id| *id == block_id.as_str())
                .ok_or_else(|| block_not_found(block_id))?;
            // `to_index` is the moved block's final index in the
            // unchanged-length list (`ReportDoc::move_block`'s contract).
            let to_index = match anchor {
                ProposalAnchor::AtStart => 0,
                ProposalAnchor::AtEnd => ids.len().saturating_sub(1),
                ProposalAnchor::AfterBlockId(target) => {
                    let target_idx = resolve_anchor_index(target, &ids, minted, deleted)?;
                    if target_idx == from {
                        return Err(CalmError::BadRequest(
                            "a block cannot be anchored after itself".into(),
                        ));
                    }
                    // move_block removes the block first, so the anchor
                    // shifts left by one when it sat after the block.
                    let shifted = if target_idx > from {
                        target_idx - 1
                    } else {
                        target_idx
                    };
                    shifted + 1
                }
            };
            Ok(ReportDocOp::MoveBlock {
                id: block_id.clone(),
                to_index,
                if_rev: Some(*if_rev),
            })
        }
        ProposalOp::DeleteBlock { block_id, if_rev } => {
            ensure_not_self_deleted(block_id, deleted)?;
            Ok(ReportDocOp::DeleteBlock {
                id: block_id.clone(),
                if_rev: *if_rev,
            })
        }
    }
}

/// Resolve an `after_block_id` target to its current list index,
/// translating a `temp:<id>` reference through the batch's minted-id map.
fn resolve_anchor_index(
    target: &str,
    ids: &[&str],
    minted: &HashMap<String, String>,
    deleted: &HashSet<String>,
) -> Result<usize, CalmError> {
    let resolved: &str = match target.strip_prefix(TEMP_REF_PREFIX) {
        Some(temp) => minted.get(temp).map(String::as_str).ok_or_else(|| {
            // Structural: no snapshot can make a backward reference to a
            // temp id this proposal never creates resolvable.
            CalmError::BadRequest(format!(
                "anchor references `{target}`, but no earlier op in this proposal created \
                 a block with that temp_id"
            ))
        })?,
        None => target,
    };
    ensure_not_self_deleted(resolved, deleted)?;
    ids.iter()
        .position(|id| *id == resolved)
        // Vanished anchor target = snapshot decay ⇒ the caller's
        // classifier maps this to the proposal-stale class (§5.2.1).
        .ok_or_else(|| block_not_found(resolved))
}

/// A block an EARLIER op of this same batch deleted is a STRUCTURAL
/// error, not staleness (§5.2.1's error split).
///
/// The distinction is the whole point of the split: "absent from the base
/// document" is snapshot decay — re-read the report, re-propose, and it
/// can succeed. "This proposal deleted it two ops ago" is
/// self-contradictory: no snapshot in any future makes it applicable, so
/// telling the plugin `stale` ("retry against a fresh baseline") would
/// invite endless resubmission of something that can never apply.
fn ensure_not_self_deleted(id: &str, deleted: &HashSet<String>) -> Result<(), CalmError> {
    if deleted.contains(id) {
        return Err(CalmError::BadRequest(format!(
            "block `{id}` was deleted by an earlier op of this proposal — no re-read can \
             make this proposal applicable; drop one of the two ops"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
