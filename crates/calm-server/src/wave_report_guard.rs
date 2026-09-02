//! #960 PR3 — fence validation + the prose-shim stomp guard for the
//! wave-report write paths.
//!
//! These checks run **inside** the persist transaction, from
//! `wave_report::apply_report_op`, so they see the CRDT truth
//! (`validate_body_fences` has one further call site outside
//! `apply_report_op` — the fork exit, noted below):
//!
//! * [`validate_body_fences`] — refuses malformed ```` ```neige-block ````
//!   fences (the lenient read would silently persist them as prose) and
//!   schema-invalid fence payloads. Its `apply_report_op` call sites are
//!   the two whole-body arms: `Replace` (`calm.report.write`/`edit` + the
//!   REST user path) and `WriteMarkdown`. It is also called outside
//!   `apply_report_op` on the fork exit (`routes::waves::
//!   prepare_fork_report`, #1252 S0b).
//! * [`validate_prose_block_content`] — since #1269, the prose
//!   `UpsertBlock` arm (both the create `id: None` and the replace
//!   `id: Some(..)` branch). It defers to
//!   `calm_types::report_blocks::check_prose_markdown`, the same rule the
//!   MCP and REST block surfaces apply, which is *stricter* than
//!   `validate_body_fences`: prose may not carry a `neige-block` fence at
//!   all, well-formed or not.
//! * [`guard_non_prose_stomp`] — only the `Replace` shim: it may not
//!   modify or delete a non-prose block; a whole-document rewrite
//!   that carries every fence through byte-for-byte passes.
//!
//! What is deliberately *not* claimed here: the **non-prose**
//! `UpsertBlock` arm also carries markdown (a fence body) and is not
//! covered by either fence check. `ReportDoc::upsert_block` only runs
//! `parse_fence` + a kind match on it, never `validate_payload`, so at the
//! op layer a schema-invalid payload inside an otherwise well-formed fence
//! is accepted on that arm — while the identical bytes through `Replace` /
//! `WriteMarkdown` are rejected. The MCP and REST surfaces close that gap
//! ahead of the op by building non-prose content with
//! `report_blocks::render_data_block`, which does schema-validate.
//!
//! All three surface `CalmError::BadRequest`, which the MCP layers map to
//! `-32602` and REST maps to 400 — the tx aborts, nothing is written,
//! no events are emitted.

use crate::error::CalmError;
use crate::wave_report_doc::ReportDoc;
use calm_types::report_blocks::{
    KIND_PROSE, check_prose_markdown, flat_text, invalid_neige_fences, parse_fence, reassign_ids,
    split_body, validate_payload,
};

/// Refuse malformed `neige-block` fences and schema-invalid fence
/// payloads anywhere in `body`.
pub(crate) fn validate_body_fences(body: &str) -> Result<(), CalmError> {
    let invalid = invalid_neige_fences(body);
    if let Some(first) = invalid.first() {
        return Err(CalmError::BadRequest(format!(
            "{first} — fix the fence or remove it (see calm.report.blocks.kinds for payload \
             schemas)"
        )));
    }
    for slice in split_body(body) {
        if let Some(fence) = parse_fence(&slice.raw) {
            validate_payload(&fence.kind, &fence.payload).map_err(|errors| {
                CalmError::BadRequest(format!(
                    "invalid `{}` block payload: {errors} (see calm.report.blocks.kinds)",
                    fence.kind
                ))
            })?;
        }
    }
    Ok(())
}

/// #1269 — the prose `UpsertBlock` entrance. For `kind == "prose"`,
/// apply the surfaces' prose rule verbatim by calling
/// [`calm_types::report_blocks::check_prose_markdown`]: prose may not
/// embed a `neige-block` fence at all. Non-prose kinds are left to
/// `ReportDoc::upsert_block`'s own canonical-fence check.
///
/// Deliberately stricter than [`validate_body_fences`], which tolerates
/// a well-formed, schema-valid fence. The op layer must not be weaker
/// than the invariant its neighbours rely on: a well-formed fence inside
/// a prose block is invisible to `ReportDoc::blocks_snapshot` (prose
/// projects as `{"markdown": text}`, so `guard_task_declarations`'
/// `is_task` never sees it), and the next wholesale write splinters it
/// into a live block — which `guard_non_prose_stomp` cannot object to,
/// because it early-returns while all *current* blocks are prose.
pub(crate) fn validate_prose_block_content(kind: &str, content: &str) -> Result<(), CalmError> {
    if kind == KIND_PROSE {
        check_prose_markdown(content).map_err(CalmError::BadRequest)?;
    }
    Ok(())
}

/// The prose-shim stomp guard: `calm.report.write` / `calm.report.edit`
/// (and the REST user path — all `Replace` ops) may not modify or
/// delete a non-prose block. Alignment is simulated exactly as
/// [`ReportDoc::update`] will land it; every existing non-prose block
/// must come out id-matched with its kind and canonical fence
/// byte-identical (a whole-document rewrite that carries the fences
/// through verbatim passes). Violations abort the tx with
/// `BadRequest` — never a silent block wipe.
pub(crate) fn guard_non_prose_stomp(doc: &ReportDoc, body: &str) -> Result<(), CalmError> {
    let current = doc
        .blocks_snapshot()
        .map_err(|e| CalmError::Internal(format!("wave_report: snapshot for stomp guard: {e}")))?;
    if current.iter().all(|block| block.kind == "prose") {
        return Ok(());
    }
    let aligned = reassign_ids(&current, &split_body(body));
    for old in current.iter().filter(|block| block.kind != "prose") {
        let preserved = aligned.iter().any(|new| {
            new.id == old.id && new.kind == old.kind && flat_text(new) == flat_text(old)
        });
        if !preserved {
            return Err(CalmError::BadRequest(format!(
                "this write would modify or delete non-prose block {} (kind {}) — the prose \
                 write/edit path may not touch data blocks; use calm.report.blocks.upsert / \
                 .delete with if_rev (task deletion must use the block-level DELETE path), or \
                 calm.report.write_markdown for a whole-document \
                 rewrite, and keep unrelated ```neige-block fences byte-identical",
                old.id, old.kind
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::error::CalmError;
    use crate::event::EditAuthor;
    use crate::wave_report::{ReportDocOp, WaveReportPayload, apply_report_op};
    use crate::wave_report_doc::ReportDoc;
    use serde_json::json;

    /// A doc holding `# A` prose + one `app` fence block; returns the
    /// doc, the fence's canonical text, and the fence block id.
    fn doc_with_app_block() -> (ReportDoc, String, String) {
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
        let fence_text = calm_types::report_blocks::render_fence(
            "app",
            &json!({ "src": "/apps/x", "height": 480 }),
        );
        let (id, _) = doc.upsert_block(None, "app", &fence_text).unwrap();
        (doc, fence_text, id)
    }

    #[test]
    fn replace_that_stomps_a_non_prose_block_is_refused() {
        // Deleting the fence, editing its JSON, or overwriting it with
        // prose must all fail BadRequest and leave the doc untouched.
        let (mut doc, fence_text, id) = doc_with_app_block();
        let before = doc.project().unwrap();

        let attempts = [
            // Fence dropped entirely.
            "# A\n\nalpha edited\n".to_string(),
            // Fence parameter edited through the prose path.
            fence_text.replace("480", "481"),
            // Fence replaced by a plain code fence of similar shape.
            "# A\n\nalpha\n```text\n{\"src\": \"/apps/other\"}\n```\n".to_string(),
        ];
        for body in &attempts {
            let err = apply_report_op(
                &mut doc,
                &ReportDocOp::Replace {
                    summary: None,
                    body: body.clone(),
                    if_doc_rev: 0,
                },
                EditAuthor::Spec,
            )
            .unwrap_err();
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m.contains(&id)
                    && m.contains("blocks.upsert")),
                "body {body:?} → {err:?}"
            );
            assert_eq!(
                doc.project().unwrap(),
                before,
                "guarded write must not land"
            );
        }
    }

    #[test]
    fn replace_preserving_the_fence_byte_for_byte_passes() {
        let (mut doc, fence_text, id) = doc_with_app_block();
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: format!("# A\n\nalpha rewritten\n{fence_text}# B\n\nnew section\n"),
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
        )
        .unwrap();
        let blocks = doc.blocks_snapshot().unwrap();
        let fence = blocks.iter().find(|b| b.id == id).expect("fence survives");
        assert_eq!(fence.kind, "app");
        assert_eq!(fence.rev, 1, "byte-preserved fence: rev holds");
        assert_eq!(fence.payload, json!({ "src": "/apps/x", "height": 480 }));
    }

    #[test]
    fn malformed_or_schema_invalid_fences_are_rejected_on_every_write_end() {
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n"));
        // Malformed fence (bad JSON): Replace and WriteMarkdown both
        // refuse instead of persisting it as prose.
        let bad_json = "# A\n```neige-block app\nnot json\n```\n";
        for op in [
            ReportDocOp::Replace {
                summary: None,
                body: bad_json.into(),
                if_doc_rev: 0,
            },
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: bad_json.into(),
                if_doc_rev: 0,
            },
        ] {
            let err = apply_report_op(&mut doc, &op, EditAuthor::Spec).unwrap_err();
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m.contains("neige-block")),
                "{err:?}"
            );
        }
        // Well-formed fence, invalid payload schema: refused with the
        // kind + field in the message.
        let bad_schema = "```neige-block chart.candles\n{\"symbol\": \"X\"}\n```\n";
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: bad_schema.into(),
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("chart.candles")
                && m.contains("candles: required")),
            "{err:?}"
        );
        // Unknown kind in a fence: refused too.
        let unknown = "```neige-block metrics\n{\"x\": 1}\n```\n";
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: unknown.into(),
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("unknown block kind")),
            "{err:?}"
        );
        assert_eq!(doc.project().unwrap().1, "# A\n", "nothing landed");
    }

    /// #1269 — the block-level entrance. `ReportDoc::upsert_block`
    /// fence-checks only NON-prose content, so a prose upsert carrying a
    /// ```` ```neige-block ```` fence used to land verbatim. Both arms are
    /// covered: creating a block (`id: None`) and replacing an existing one
    /// (`id: Some(..)` + `if_rev`).
    ///
    /// All three fence shapes are refused, because the op layer applies the
    /// surfaces' `check_prose_markdown` rule (no fence in prose at all), not
    /// the weaker `validate_body_fences`:
    ///
    /// 1. malformed (unparseable JSON interior),
    /// 2. well-formed but schema-invalid — the case that a check built only
    ///    on `invalid_neige_fences` would wave through,
    /// 3. well-formed *and* schema-valid — refused because a fence hidden in
    ///    a prose block is invisible to `blocks_snapshot` and splinters into
    ///    a live block on the next wholesale write.
    #[test]
    fn prose_upsert_with_any_neige_fence_is_refused_on_both_arms() {
        let well_formed_valid = calm_types::report_blocks::render_fence(
            "app",
            &json!({ "src": "/apps/x", "height": 480 }),
        );
        let cases: [(&str, String); 3] = [
            (
                "malformed",
                "# A\n```neige-block app\nnot json\n```\n".into(),
            ),
            (
                "well-formed, schema-invalid",
                "# A\n```neige-block chart.candles\n{\"symbol\": \"X\"}\n```\n".into(),
            ),
            (
                "well-formed, schema-valid",
                format!("# A\n\n{well_formed_valid}"),
            ),
        ];

        for (label, content) in cases {
            // Create arm.
            let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
            let before = doc.project().unwrap();
            let err = match apply_report_op(
                &mut doc,
                &ReportDocOp::UpsertBlock {
                    id: None,
                    kind: "prose".into(),
                    content: content.clone(),
                    if_rev: None,
                    if_doc_rev: Some(0),
                    position: None,
                },
                EditAuthor::Spec,
            ) {
                Ok(landed) => panic!("{label}: create arm must refuse the fence, got {landed:?}"),
                Err(err) => err,
            };
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m.contains("neige-block")),
                "{label}: {err:?}"
            );
            assert_eq!(
                doc.project().unwrap(),
                before,
                "{label}: create must not land"
            );

            // Replace arm: the existing prose block, at its current rev.
            let block = doc.blocks_snapshot().unwrap().remove(0);
            assert_eq!(block.kind, "prose");
            let err = match apply_report_op(
                &mut doc,
                &ReportDocOp::UpsertBlock {
                    id: Some(block.id.clone()),
                    kind: "prose".into(),
                    content: content.clone(),
                    if_rev: Some(block.rev),
                    if_doc_rev: None,
                    position: None,
                },
                EditAuthor::Spec,
            ) {
                Ok(landed) => panic!("{label}: replace arm must refuse the fence, got {landed:?}"),
                Err(err) => err,
            };
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m.contains("neige-block")),
                "{label}: {err:?}"
            );
            assert_eq!(
                doc.project().unwrap(),
                before,
                "{label}: replace must not land"
            );
        }
    }

    /// The scope fence for the check above: it must refuse `neige-block`
    /// fences, not ordinary markdown. Headings, lists and a plain
    /// ```` ```rust ```` code fence still land on both arms.
    #[test]
    fn fence_free_prose_upsert_still_lands_on_both_arms() {
        let body = "# Notes\n\n- alpha\n- beta\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n";

        // Create arm.
        let mut doc = ReportDoc::from_payload(&WaveReportPayload::new("s", "# A\n\nalpha\n"));
        apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: None,
                kind: "prose".into(),
                content: body.into(),
                if_rev: None,
                if_doc_rev: Some(0),
                position: None,
            },
            EditAuthor::Spec,
        )
        .expect("fence-free prose must be accepted on the create arm");
        assert!(
            doc.project().unwrap().1.contains(body),
            "create must land: {:?}",
            doc.project().unwrap().1
        );

        // Replace arm: overwrite the first prose block with the same body.
        let block = doc.blocks_snapshot().unwrap().remove(0);
        assert_eq!(block.kind, "prose");
        apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some(block.id.clone()),
                kind: "prose".into(),
                content: body.into(),
                if_rev: Some(block.rev),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Spec,
        )
        .expect("fence-free prose must be accepted on the replace arm");
        assert!(
            doc.project().unwrap().1.contains(body),
            "replace must land: {:?}",
            doc.project().unwrap().1
        );
    }

    #[test]
    fn write_markdown_may_edit_fence_params_and_bumps_only_that_block() {
        // The escape hatch is allowed to change data blocks: editing
        // the fence JSON bumps that block's rev, the rest hold.
        let (mut doc, fence_text, id) = doc_with_app_block();
        let body = format!("# A\n\nalpha\n{}", fence_text.replace("480", "600"));
        apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body,
                if_doc_rev: 0,
            },
            EditAuthor::Spec,
        )
        .unwrap();
        let blocks = doc.blocks_snapshot().unwrap();
        assert_eq!(blocks[0].rev, 1, "prose untouched");
        let fence = blocks.iter().find(|b| b.id == id).expect("id survives");
        assert_eq!(fence.rev, 2, "edited fence: rev+1");
        assert_eq!(fence.payload, json!({ "src": "/apps/x", "height": 600 }));
        // Observation distinguishability at the doc level: the two
        // parameterizations project different bodies.
        assert_ne!(doc.project().unwrap().1, {
            let (doc_before, _, _) = doc_with_app_block();
            doc_before.project().unwrap().1
        });
    }
}
