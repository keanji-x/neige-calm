//! Apply report operations and guard their final block diff.
use super::*;

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
#[cfg(test)]
pub(crate) fn apply_report_op_traced(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<ReportOpTrace, CalmError> {
    apply_with_knowledge(doc, op, author, None).map_err(Into::into)
}

fn apply_with_knowledge(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
    mut knowledge: Option<crate::report_read_ledger::LastRead>,
) -> Result<ReportOpTrace, ReportError> {
    let raw_ops = match op {
        ReportDocOp::AgentBatch { ops, .. } => Some(ops),
        _ => None,
    };
    let bound_header = super::intent::header(op, knowledge.as_ref())
        .map_err(|e| e.freeze(doc, knowledge.as_ref()))?;
    let op = &bound_header;
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
    let mut written_block_ids = Vec::new();
    let mut section_output_ids = Vec::new();

    let mut bound_steps = Vec::new();
    let outcome: Result<Option<BlockOpOutcome>, ReportError> = match &op {
        ReportDocOp::AgentBatch { .. } | ReportDocOp::AgentWrite { .. } => {
            unreachable!("intent header bound")
        }
        ReportDocOp::Replace {
            summary,
            body,
            if_doc_rev,
        } => {
            check_doc_rev(doc, *if_doc_rev)?;
            // Line 1 is rewritten to the canonical header before anything reads the body, so every check
            // and the doc write see the same bytes the funnel will.
            let body = normalize_header(body).map_err(header_bad_request)?;
            validate_body_fences(&body)?;
            let slices = calm_types::report_blocks::split_body(&body);
            if slices
                .iter()
                .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
                .any(|fence| fence.kind == calm_types::report_blocks::KIND_OBJECTIVE)
            {
                return Err(CalmError::BadRequest(
                    "a whole write may not carry objective fences; use an upsert by id".into(),
                )
                .into());
            }
            guard_non_prose_stomp(doc, &body)?;
            doc.update_with_hints_keeping(summary, &slices, &vec![None; slices.len()], |block| {
                block.kind != calm_types::report_blocks::KIND_OBJECTIVE
            })
            .map_err(internal)?;
            written = Written::AllProse;
            Ok(None)
        }
        ReportDocOp::WriteMarkdown {
            summary,
            body,
            if_doc_rev,
        } => {
            check_doc_anchor(doc, DocAnchor::LastRead(*if_doc_rev), true).map_err(|e| {
                let e = match e {
                    ReportError::Anchor { source, reason, .. } => {
                        ReportError::anchor(source, reason, Scope::Whole)
                    }
                    other => other,
                };
                e.freeze(doc, knowledge.as_ref())
            })?;
            let summary = tx_summary(doc, summary)?;
            let mut marked = calm_types::report_blocks::strip_markers_and_split(body);
            // Normalize AFTER the markers are stripped: a `with_markers` read puts the marker on line 1
            // and the header on line 2. The header is line 1 of slice 0, so rewriting it there keeps
            // every block boundary and its hint.
            if let Some(first) = marked.slices.first_mut()
                && let Cow::Owned(normalized) =
                    normalize_header(&first.raw).map_err(header_bad_request)?
            {
                first.raw = normalized;
                marked.cleaned = calm_types::report_blocks::flatten(&marked.slices);
            }
            let slices = &marked.slices;
            // A whole write owns the blocks a full read renders, never a task block (#2617): those
            // stay as they are, so the markdown may neither carry nor pin one.
            sections::refuse_unsectioned_fences(slices, "the markdown of a whole write")?;
            let current = doc.blocks_snapshot().map_err(internal)?;
            if let Some((id, kind)) = marked.hints.iter().flatten().find_map(|id| {
                current
                    .iter()
                    .find(|block| &block.id == id && !in_sections(&block.kind))
                    .map(|block| (id, &block.kind))
            }) {
                return Err(CalmError::BadRequest(format!(
                    "marker `<!-- neige:{id} -->` names a {kind} block; a whole write keeps every \
                     {kind} block as it is — change one with an `upsert` op after reading it by id"
                ))
                .into());
            }
            // Every fence it does carry must be well-formed and schema-valid.
            validate_marked_slices(&marked.cleaned, slices)?;
            doc.update_with_hints_keeping(&summary, slices, &marked.hints, |block| {
                in_sections(&block.kind)
            })
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
                    apply_upsert_new(doc, kind, content, *position, validate).map_err(Into::into)
                }
            };
            outcome.map(|outcome| {
                written_block_ids.push(outcome.id.clone());
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
        ReportDocOp::ResetToInitial { if_doc_rev } => {
            // Only the user's reset door builds this op; any other author is a kernel bug.
            if author != EditAuthor::User {
                return Err(CalmError::Internal(format!(
                    "track_report: ResetToInitial is the user's reset; {author:?} may not apply it"
                ))
                .into());
            }
            check_doc_rev(doc, *if_doc_rev)?;
            // Every data block is dropped and none can lend its id or kind to the initial prose. A
            // later write may mint a dropped id again, as after a DELETE and a DELETE of its tombstone.
            let initial = TrackReportPayload::initial();
            doc.replace_dropping_data_blocks(&initial.summary, &initial.body)
                .map_err(internal)?;
            written = Written::AllProse;
            Ok(None)
        }
        ReportDocOp::Batch {
            doc_anchor,
            summary,
            ops,
        } => {
            let needs_doc_anchor = summary.is_some()
                || ops.iter().any(|op| {
                    matches!(
                        op,
                        BatchBlockOp::Upsert { id: None, .. } | BatchBlockOp::Move { .. }
                    )
                });
            let needs_doc_anchor = needs_doc_anchor
                || raw_ops.is_some_and(|ops| {
                    ops.iter().any(|op| {
                        matches!(
                            op,
                            BlockIntent::Upsert { id: None, .. } | BlockIntent::Move { .. }
                        )
                    })
                });
            check_doc_anchor(doc, *doc_anchor, needs_doc_anchor)
                .map_err(|e| e.freeze(doc, knowledge.as_ref()))?;
            if raw_ops.map_or(ops.len(), Vec::len) > MAX_BATCH_OPS {
                return Err(CalmError::BadRequest(format!(
                    "batch carries {} ops; at most {MAX_BATCH_OPS} per commit",
                    ops.len()
                ))
                .into());
            }
            // Ops run in order against the doc as the previous ones left it; batches address existing
            // blocks (a created id is not known to the caller yet). The first `?` aborts the whole tx.
            let mut written_ids = Vec::new();
            for index in 0..raw_ops.map_or(ops.len(), Vec::len) {
                let step_before = doc.blocks_snapshot().map_err(internal)?;
                let written_start = written_block_ids.len();
                let section_start = section_output_ids.len();
                let result = (|| -> Result<(), ReportError> {
                    let block_op = if let Some(raw) = raw_ops {
                        raw[index].bind(knowledge.as_ref())?
                    } else {
                        ops[index].clone()
                    };
                    bound_steps.push(block_op.clone());
                    let block_op = &block_op;
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
                                    let outcome = apply_upsert_existing(
                                        doc, id, kind, content, expected, true,
                                    )?;
                                    written_block_ids.push(outcome.id.clone());
                                    outcome
                                }
                                None => {
                                    let outcome =
                                        apply_upsert_new(doc, kind, content, *position, true)?;
                                    written_block_ids.push(outcome.id.clone());
                                    outcome
                                }
                            };
                            if kind == KIND_PROSE {
                                written_ids.push(outcome.id);
                            }
                        }
                        BatchBlockOp::Move { id, to_index } => {
                            apply_move(doc, id, *to_index)?;
                        }
                        BatchBlockOp::Delete { id, if_rev } => {
                            apply_delete(doc, id, *if_rev)?;
                        }
                        BatchBlockOp::ReplaceSection {
                            section,
                            markdown,
                            read,
                        } => {
                            let (replaced, _, _) = sections::apply_replace_section(
                                doc,
                                section,
                                markdown,
                                read,
                                *doc_anchor,
                            )?;
                            written_ids.extend(
                                replaced
                                    .iter()
                                    .filter(|block| block.kind == KIND_PROSE)
                                    .map(|block| block.id.clone()),
                            );
                            section_output_ids
                                .extend(replaced.iter().map(|block| block.id.clone()));
                            written_block_ids.extend(replaced.into_iter().map(|block| block.id));
                        }
                        BatchBlockOp::DeleteSection { section, read } => {
                            sections::apply_delete_section(doc, section, read)?;
                        }
                    }
                    Ok(())
                })();
                result.map_err(|e| e.freeze(doc, knowledge.as_ref()).at_step(index))?;
                if let Some(old) = knowledge.as_ref() {
                    let rev = doc.doc_rev().map_err(internal)?;
                    knowledge = Some(crate::report_read_ledger::project_commit(
                        old,
                        rev,
                        &step_before,
                        rev,
                        &doc.blocks_snapshot().map_err(internal)?,
                        &written_block_ids[written_start..],
                        &section_output_ids[section_start..],
                    ));
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
    if matches!(&op, ReportDocOp::WriteMarkdown { .. }) {
        // The task blocks it kept were not written: they stay unknown to a session that never read them.
        let rewritten = after.iter().filter(|block| in_sections(&block.kind));
        written_block_ids.extend(rewritten.clone().map(|block| block.id.clone()));
        section_output_ids.extend(rewritten.map(|block| block.id.clone()));
    }
    // A delete that names the block by id is the ONLY way a live task declaration may leave the
    // document. `op` is the normalized op, so a user delete rewritten into a tombstone grants no exemption.
    let deleted_by_id: Vec<&str> = match &op {
        ReportDocOp::DeleteBlock { id, .. } => vec![id.as_str()],
        ReportDocOp::ResetToInitial { .. } => before
            .iter()
            .filter(|block| block.kind != KIND_PROSE)
            .map(|block| block.id.as_str())
            .collect(),
        ReportDocOp::Batch { .. } => bound_steps
            .iter()
            .filter_map(|op| match op {
                BatchBlockOp::Delete { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    super::compact_origination::guard_compact_origination(&before, &after, author)?;
    guard_task_declarations(&before, &after, author, &deleted_by_id)?;
    crate::track_report_objective_guard::guard_objective_blocks(&before, &after, author)?;
    let knowledge = knowledge.as_ref().map(|old| {
        let rev = doc.doc_rev().expect("revision already checked");
        if raw_ops.is_some() {
            old.clone()
        } else {
            crate::report_read_ledger::project_commit(
                old,
                rev,
                &before,
                rev,
                &after,
                &written_block_ids,
                &section_output_ids,
            )
        }
    });
    Ok(ReportOpTrace {
        block: outcome,
        written_prose_block_ids,
        written_block_ids,
        section_output_ids,
        knowledge,
    })
}

/// Advance the document-wide revision exactly once per successful op. Kept outside
/// [`apply_report_op`]: every operation invalidates whole-document anchors, including moves
/// and content-equal replacements that do not bump a block.
#[cfg(test)]
pub(super) fn apply_persisted_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
) -> Result<(ReportOpTrace, u64), CalmError> {
    apply_persisted_with_knowledge(doc, op, author, None).map_err(Into::into)
}

pub(super) fn apply_persisted_with_knowledge(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
    author: EditAuthor,
    knowledge: Option<crate::report_read_ledger::LastRead>,
) -> Result<(ReportOpTrace, u64), ReportError> {
    let before_rev = doc.doc_rev().map_err(block_op_internal)?;
    let mut trace = apply_with_knowledge(doc, op, author, knowledge)?;
    let doc_rev = doc.increment_doc_rev().map_err(|e| {
        CalmError::Internal(format!("track_report: increment document revision: {e}"))
    })?;
    if let Some(read) = trace.knowledge.as_mut() {
        read.whole = read.whole && read.doc_rev == before_rev;
        if read.doc_rev == before_rev {
            read.doc_rev = doc_rev;
        }
    }
    Ok((trace, doc_rev))
}
