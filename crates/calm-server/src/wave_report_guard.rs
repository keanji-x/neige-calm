//! #960 PR3 — fence validation + the prose-shim stomp guard for the
//! wave-report write paths.
//!
//! Both checks run **inside** the persist transaction, from
//! `wave_report::apply_report_op`, so they see the CRDT truth:
//!
//! * [`validate_body_fences`] — every write end that accepts a whole
//!   markdown body (`Replace` — `calm.report.write`/`edit` + the REST
//!   user path — and `WriteMarkdown`) must refuse malformed
//!   ```` ```neige-block ```` fences (the lenient read would silently
//!   persist them as prose) and schema-invalid fence payloads.
//! * [`guard_non_prose_stomp`] — only the `Replace` shim: it may not
//!   modify or delete a non-prose block; a whole-document rewrite
//!   that carries every fence through byte-for-byte passes.
//!
//! Both surface `CalmError::BadRequest`, which the MCP layers map to
//! `-32602` and REST maps to 400 — the tx aborts, nothing is written,
//! no events are emitted.

use crate::error::CalmError;
use crate::wave_report_doc::ReportDoc;
use calm_types::report_blocks::{
    flat_text, invalid_neige_fences, parse_fence, reassign_ids, split_body, validate_payload,
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
                 .delete with if_rev, or calm.report.write_markdown for a whole-document \
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
                    if_rev: 0,
                },
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
                if_rev: 0,
            },
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
                if_rev: 0,
            },
            ReportDocOp::WriteMarkdown {
                summary: None,
                body: bad_json.into(),
                if_rev: 0,
            },
        ] {
            let err = apply_report_op(&mut doc, &op).unwrap_err();
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
                if_rev: 0,
            },
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
                if_rev: 0,
            },
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("unknown block kind")),
            "{err:?}"
        );
        assert_eq!(doc.project().unwrap().1, "# A\n", "nothing landed");
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
                if_rev: 0,
            },
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
