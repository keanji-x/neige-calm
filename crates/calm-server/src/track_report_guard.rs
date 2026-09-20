//! Fence validation and the prose-shim stomp guard for the track-report write paths; runs inside the
//! persist transaction and surfaces `CalmError::BadRequest`, so the tx aborts and nothing is written.

use crate::error::CalmError;
use crate::track_report_doc::ReportDoc;
use calm_types::report_blocks::{
    KIND_PROSE, NonProseFence, check_prose_markdown, flat_text, invalid_neige_fences, parse_fence,
    reassign_ids, split_body, validate_payload,
};

/// Schema-validate one parsed fence's payload as a `BadRequest` naming the kind and field errors.
fn check_fence_payload(fence: &NonProseFence) -> Result<(), CalmError> {
    validate_payload(&fence.kind, &fence.payload).map_err(|errors| {
        CalmError::BadRequest(format!(
            "invalid `{}` block payload: {errors} (see calm.report.blocks.kinds)",
            fence.kind
        ))
    })
}

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
            check_fence_payload(&fence)?;
        }
    }
    Ok(())
}

/// Content rule for the `UpsertBlock` arms, dispatched on the op's own `kind`: prose may not embed a
/// `neige-block` fence at all; any other kind that parses as one canonical fence is schema-validated.
pub(crate) fn validate_block_content(kind: &str, content: &str) -> Result<(), CalmError> {
    if kind == KIND_PROSE {
        return check_prose_markdown(content).map_err(CalmError::BadRequest);
    }
    if let Some(fence) = parse_fence(content) {
        check_fence_payload(&fence)?;
    }
    Ok(())
}

/// `Replace` ops may not modify or delete a non-prose block: every existing non-prose block must come out
/// of the simulated alignment id-matched with its kind and canonical fence byte-identical.
pub(crate) fn guard_non_prose_stomp(doc: &ReportDoc, body: &str) -> Result<(), CalmError> {
    let current = doc
        .blocks_snapshot()
        .map_err(|e| CalmError::Internal(format!("track_report: snapshot for stomp guard: {e}")))?;
    if current.iter().all(|block| block.kind == KIND_PROSE) {
        return Ok(());
    }
    let aligned = reassign_ids(&current, &split_body(body));
    for old in current.iter().filter(|block| block.kind != KIND_PROSE) {
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
    use crate::track_report::{ReportDocOp, TrackReportPayload, apply_report_op};
    use crate::track_report_doc::ReportDoc;
    use serde_json::json;

    fn doc_with_app_block() -> (ReportDoc, String, String) {
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let fence_text = calm_types::report_blocks::render_fence(
            "app",
            &json!({ "src": "/apps/x", "height": 480 }),
        );
        let (id, _) = doc.upsert_block(None, "app", &fence_text).unwrap();
        (doc, fence_text, id)
    }

    #[test]
    fn replace_that_stomps_a_non_prose_block_is_refused() {
        let (mut doc, fence_text, id) = doc_with_app_block();
        let before = doc.project().unwrap();

        let attempts = [
            "# A\n\nalpha edited\n".to_string(),
            fence_text.replace("480", "481"),
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
                EditAuthor::Planner,
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
            EditAuthor::Planner,
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
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n"));
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
            let err = apply_report_op(&mut doc, &op, EditAuthor::Planner).unwrap_err();
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m.contains("neige-block")),
                "{err:?}"
            );
        }
        let bad_schema = "```neige-block chart.candles\n{\"symbol\": \"X\"}\n```\n";
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: bad_schema.into(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("chart.candles")
                && m.contains("candles: required")),
            "{err:?}"
        );
        let unknown = "```neige-block metrics\n{\"x\": 1}\n```\n";
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: unknown.into(),
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("unknown block kind")),
            "{err:?}"
        );
        assert_eq!(doc.project().unwrap().1, "# A\n", "nothing landed");
    }

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
            let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
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
                EditAuthor::Planner,
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
                EditAuthor::Planner,
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

    /// The body mentions `` `neige-block` `` inline on purpose: only a fence *opener* counts.
    #[test]
    fn fence_free_prose_upsert_still_lands_on_both_arms() {
        let body = "# Notes\n\n- alpha\n- beta — a data block is written with a `neige-block` \
                    fence, but naming it here is prose\n\n```rust\nfn main() { \
                    println!(\"hi\"); }\n```\n";

        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
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
            EditAuthor::Planner,
        )
        .expect("fence-free prose must be accepted on the create arm");
        assert!(
            doc.project().unwrap().1.contains(body),
            "create must land: {:?}",
            doc.project().unwrap().1
        );

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
            EditAuthor::Planner,
        )
        .expect("fence-free prose must be accepted on the replace arm");
        assert!(
            doc.project().unwrap().1.contains(body),
            "replace must land: {:?}",
            doc.project().unwrap().1
        );
    }

    /// The rejection must be `BadRequest`, not `Internal`, which is why the check cannot live inside `upsert_block`.
    #[test]
    fn non_prose_upsert_with_a_schema_invalid_payload_is_refused_on_both_arms() {
        let content =
            calm_types::report_blocks::render_fence("chart.candles", &json!({ "symbol": "X" }));

        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let before = doc.project().unwrap();
        let err = match apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: None,
                kind: "chart.candles".into(),
                content: content.clone(),
                if_rev: None,
                if_doc_rev: Some(0),
                position: None,
            },
            EditAuthor::Planner,
        ) {
            Ok(landed) => panic!("create arm must refuse the payload, got {landed:?}"),
            Err(err) => err,
        };
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("chart.candles")
                && m.contains("candles: required")),
            "create arm: {err:?}"
        );
        assert_eq!(doc.project().unwrap(), before, "create must not land");

        let block = doc.blocks_snapshot().unwrap().remove(0);
        assert_eq!(block.kind, "prose");
        let err = match apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some(block.id.clone()),
                kind: "chart.candles".into(),
                content,
                if_rev: Some(block.rev),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Planner,
        ) {
            Ok(landed) => panic!("replace arm must refuse the payload, got {landed:?}"),
            Err(err) => err,
        };
        assert!(
            matches!(&err, CalmError::BadRequest(m) if m.contains("chart.candles")
                && m.contains("candles: required")),
            "replace arm: {err:?}"
        );
        assert_eq!(doc.project().unwrap(), before, "replace must not land");
    }

    #[test]
    fn schema_valid_non_prose_upsert_still_lands_on_both_arms() {
        let content = calm_types::report_blocks::render_fence(
            "app",
            &json!({ "src": "/apps/x", "height": 480 }),
        );

        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: None,
                kind: "app".into(),
                content: content.clone(),
                if_rev: None,
                if_doc_rev: Some(0),
                position: None,
            },
            EditAuthor::Planner,
        )
        .expect("a schema-valid app block must be accepted on the create arm");
        let created = outcome.expect("upsert reports the block").id;
        let block = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .find(|block| block.id == created)
            .expect("the created block is live");
        assert_eq!(block.kind, "app");
        assert_eq!(block.payload, json!({ "src": "/apps/x", "height": 480 }));

        let edited = content.replace("480", "600");
        apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some(created.clone()),
                kind: "app".into(),
                content: edited,
                if_rev: Some(block.rev),
                if_doc_rev: None,
                position: None,
            },
            EditAuthor::Planner,
        )
        .expect("a schema-valid app block must be accepted on the replace arm");
        let block = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .find(|block| block.id == created)
            .expect("the replaced block is live");
        assert_eq!(block.payload, json!({ "src": "/apps/x", "height": 600 }));
    }

    /// The synthesized tombstone op copies `key` off the stored block, so the delete's verdict must not
    /// depend on that stored payload validating against the current schema.
    #[test]
    fn user_delete_of_a_task_with_a_schema_invalid_stored_key_still_tombstones() {
        assert!(
            !calm_types::report_blocks::tasks::key_is_valid("Build"),
            "the fixture only means anything while `Build` is an invalid key"
        );
        let legacy = calm_types::report_blocks::render_fence(
            "task",
            &json!({
                "key": "Build",
                "goal": "build it",
                "kind": "codex",
                "ready": true,
                "declared_by": "user",
            }),
        );
        let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", &legacy));
        let task = doc
            .blocks_snapshot()
            .unwrap()
            .into_iter()
            .find(|block| block.kind == "task")
            .expect("the seeded task block projects");
        assert_eq!(task.payload["key"], "Build");
        assert!(
            task.payload.get("tombstone").is_none(),
            "the seeded block is live, not already retired"
        );

        apply_report_op(
            &mut doc,
            &ReportDocOp::DeleteBlock {
                id: task.id.clone(),
                if_rev: task.rev,
            },
            EditAuthor::User,
        )
        .expect("a user must still be able to retire a task whose stored key is invalid");

        let after = doc.blocks_snapshot().unwrap();
        let tombstone = after
            .iter()
            .find(|block| block.id == task.id)
            .expect("the block is retired in place, not dropped");
        assert_eq!(tombstone.kind, "task");
        assert_eq!(tombstone.payload["tombstone"], json!({ "reason": null }));
        assert_eq!(tombstone.payload["tombstoned_by"], "user");
        assert_eq!(tombstone.payload["key"], "Build");
    }

    /// Projection keeps prose fragments of a fence on separate lines; a caller who concatenates them
    /// manually still hits the materialising Replace's task-attribution guard.
    #[test]
    fn fence_assembled_across_two_prose_blocks_is_caught_at_the_materialising_write() {
        use calm_types::report_blocks::{check_prose_markdown, parse_fence, split_body};

        fn stage_split_fence(fence: &str, at: usize) -> (ReportDoc, String, Vec<String>) {
            let (head, tail) = fence.split_at(at);
            let a = format!("# A\n\nalpha\n{head}");
            let b = format!("{tail}# B\n\nbeta\n");
            assert_eq!(
                format!("{a}{b}"),
                format!("# A\n\nalpha\n{fence}# B\n\nbeta\n"),
                "the fragments must concatenate back to the fence"
            );
            check_prose_markdown(&a).expect("fragment A is accepted prose");
            check_prose_markdown(&b).expect("fragment B is accepted prose");

            let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "seed\n"));
            let seed = doc.blocks_snapshot().unwrap().remove(0);
            apply_report_op(
                &mut doc,
                &ReportDocOp::UpsertBlock {
                    id: Some(seed.id.clone()),
                    kind: "prose".into(),
                    content: a,
                    if_rev: Some(seed.rev),
                    if_doc_rev: None,
                    position: None,
                },
                EditAuthor::Planner,
            )
            .expect("fragment A lands on the replace arm");
            apply_report_op(
                &mut doc,
                &ReportDocOp::UpsertBlock {
                    id: None,
                    kind: "prose".into(),
                    content: b,
                    if_rev: None,
                    if_doc_rev: Some(0),
                    position: None,
                },
                EditAuthor::Planner,
            )
            .expect("fragment B lands on the create arm");

            let kinds = doc
                .blocks_snapshot()
                .unwrap()
                .iter()
                .map(|block| block.kind.clone())
                .collect();
            let (_, body) = doc.project().unwrap();
            assert!(
                split_body(&body)
                    .iter()
                    .all(|slice| parse_fence(&slice.raw).is_none()),
                "projection must not assemble a data fence across prose blocks"
            );
            (doc, format!("# A\n\nalpha\n{fence}# B\n\nbeta\n"), kinds)
        }

        // Split right after the opener so neither fragment parses as a fence.
        let at = "```neige-block ".len();

        let app_fence = calm_types::report_blocks::render_fence(
            "app",
            &json!({ "src": "/apps/x", "height": 480 }),
        );
        let (mut doc, body, kinds) = stage_split_fence(&app_fence, at);
        assert!(
            kinds.iter().all(|kind| kind == "prose"),
            "after the two upserts the document is still all prose: {kinds:?}"
        );
        let assembled: Vec<String> = split_body(&body)
            .iter()
            .filter_map(|slice| parse_fence(&slice.raw))
            .map(|fence| fence.kind.clone())
            .collect();
        assert_eq!(
            assembled,
            vec!["app".to_string()],
            "the projection reassembles into a parseable fence: {body:?}"
        );
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body,
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .expect("the wholesale write materialises the assembled app block");
        assert!(
            doc.blocks_snapshot()
                .unwrap()
                .iter()
                .any(|block| block.kind == "app"),
            "the app block is now live — the same block a single Replace creates directly"
        );

        let task_fence = calm_types::report_blocks::render_fence(
            "task",
            &json!({
                "key": "t1",
                "goal": "g",
                "kind": "codex",
                "ready": true,
                "declared_by": "user",
            }),
        );
        let (mut doc, body, kinds) = stage_split_fence(&task_fence, at);
        assert!(
            kinds.iter().all(|kind| kind == "prose"),
            "after the two upserts the document is still all prose: {kinds:?}"
        );
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body,
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .expect_err("a Planner write may not materialise a task attributed to the user");
        let CalmError::BadRequest(message) = err else {
            panic!("expected BadRequest, got {err:?}");
        };
        assert!(
            message.contains("declared_by") && message.contains("spec"),
            "the attribution guard is what rejects it: {message}"
        );
        // The guard runs on before/after snapshots: the in-memory doc is already mutated; the caller's tx abort discards it.
        assert!(
            doc.blocks_snapshot()
                .unwrap()
                .iter()
                .any(|block| block.kind == "task"),
            "the rejection is a refused op, not an in-memory rollback"
        );
    }

    #[test]
    fn write_markdown_may_edit_fence_params_and_bumps_only_that_block() {
        let (mut doc, fence_text, id) = doc_with_app_block();
        let body = format!("# A\n\nalpha\n{}", fence_text.replace("480", "600"));
        apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body,
                if_doc_rev: 0,
            },
            EditAuthor::Planner,
        )
        .unwrap();
        let blocks = doc.blocks_snapshot().unwrap();
        assert_eq!(blocks[0].rev, 1, "prose untouched");
        let fence = blocks.iter().find(|b| b.id == id).expect("id survives");
        assert_eq!(fence.rev, 2, "edited fence: rev+1");
        assert_eq!(fence.payload, json!({ "src": "/apps/x", "height": 600 }));
        assert_ne!(doc.project().unwrap().1, {
            let (doc_before, _, _) = doc_with_app_block();
            doc_before.project().unwrap().1
        });
    }
}
