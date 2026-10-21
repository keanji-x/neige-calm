use super::*;
use calm_types::report_blocks::render_fence;
use serde_json::{Value, json};

pub(crate) fn payload() -> Value {
    json!({"key":"build","title":"Build","mode":"converge",
        "checks":[{"id":"tests","role":"goal","config":{"kind":"command","params":{"command":"make test"}}}],
        "constraints":[],"budget":{"turns":10,"tokens":1000,"wall_secs":60},"compact":"none"})
}
fn upsert(id: Option<String>, if_rev: Option<u32>, value: &Value) -> ReportDocOp {
    ReportDocOp::UpsertBlock {
        id,
        kind: "objective".into(),
        content: render_fence("objective", value),
        if_rev,
        if_doc_rev: Some(0),
        position: None,
    }
}
fn doc() -> ReportDoc {
    ReportDoc::from_payload(&TrackReportPayload::new(
        "s",
        render_fence("objective", &payload()),
    ))
}
#[test]
fn assistant_cannot_create_modify_or_delete_objective_blocks() {
    let original = doc();
    let block = original.blocks_snapshot().unwrap()[0].clone();
    let mut changed = payload();
    changed["title"] = json!("Changed");
    for author in [
        EditAuthor::Assistant,
        EditAuthor::Kernel,
        EditAuthor::Plugin,
    ] {
        for op in [
            upsert(None, None, &payload()),
            upsert(Some(block.id.clone()), Some(block.rev), &changed),
            ReportDocOp::DeleteBlock {
                id: block.id.clone(),
                if_rev: block.rev,
            },
            ReportDocOp::Batch {
                doc_anchor: super::DocAnchor::LastRead(0),
                summary: None,
                ops: vec![BatchBlockOp::Delete {
                    id: block.id.clone(),
                    if_rev: block.rev,
                }],
            },
        ] {
            let mut attempt = if matches!(&op, ReportDocOp::UpsertBlock { id: None, .. }) {
                ReportDoc::from_payload(&TrackReportPayload::new("s", ""))
            } else {
                doc()
            };
            assert!(
                apply_report_op(&mut attempt, &op, author).is_err(),
                "{author:?} {op:?}"
            );
        }
        let mut positive = payload();
        positive["checks"][0]["config"] = json!({"kind":"metric","params":{"command":"bench","direction":"lower","tolerance":0.0}});
        let mut negative = positive.clone();
        negative["checks"][0]["config"]["params"]["tolerance"] = json!(-0.0);
        assert_ne!(
            render_fence("objective", &positive),
            render_fence("objective", &negative)
        );
        let mut floats = ReportDoc::from_payload(&TrackReportPayload::new(
            "s",
            render_fence("objective", &positive),
        ));
        let block = floats.blocks_snapshot().unwrap()[0].clone();
        assert!(
            apply_report_op(
                &mut floats,
                &upsert(Some(block.id), Some(block.rev), &negative),
                author
            )
            .is_err(),
            "{author:?} may not change a serialized objective standard"
        );
    }
}
#[test]
fn objective_key_and_kind_are_immutable() {
    let original = doc();
    let block = original.blocks_snapshot().unwrap()[0].clone();
    for author in [EditAuthor::Planner, EditAuthor::User] {
        let mut renamed = payload();
        renamed["key"] = json!("renamed");
        for op in [
            upsert(Some(block.id.clone()), Some(block.rev), &renamed),
            ReportDocOp::UpsertBlock {
                id: Some(block.id.clone()),
                kind: "app".into(),
                content: render_fence("app", &json!({"src":"/x"})),
                if_rev: Some(block.rev),
                if_doc_rev: None,
                position: None,
            },
        ] {
            let error = apply_report_op(&mut doc(), &op, author).unwrap_err();
            assert!(matches!(error,CalmError::BadRequest(reason) if reason.contains("immutable")));
        }
        let mut changed = payload();
        changed["title"] = json!("Changed");
        apply_report_op(
            &mut doc(),
            &upsert(Some(block.id.clone()), Some(block.rev), &changed),
            author,
        )
        .unwrap();
    }
}
#[test]
fn whole_write_keeps_objective_blocks_and_refuses_their_fences() {
    let mut original = doc();
    let before = original.blocks_snapshot().unwrap();
    apply_report_op(
        &mut original,
        &ReportDocOp::WriteMarkdown {
            summary: None,
            body: "# changed\n".into(),
            if_doc_rev: 0,
        },
        EditAuthor::Assistant,
    )
    .unwrap();
    assert!(original.blocks_snapshot().unwrap().contains(&before[0]));
    assert!(
        apply_report_op(
            &mut doc(),
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: render_fence("objective", &payload()),
                if_doc_rev: 0
            },
            EditAuthor::Planner
        )
        .is_err()
    );
}

#[test]
fn objective_legacy_whole_replace_keeps_blocks_and_refuses_fences() {
    let mut original = doc();
    let before = original.blocks_snapshot().unwrap();
    apply_report_op(
        &mut original,
        &ReportDocOp::Replace {
            summary: "s".into(),
            body: "# Changed\n".into(),
            if_doc_rev: 0,
        },
        EditAuthor::User,
    )
    .unwrap();
    assert!(original.blocks_snapshot().unwrap().contains(&before[0]));
    assert!(
        apply_report_op(
            &mut doc(),
            &ReportDocOp::Replace {
                summary: "s".into(),
                body: render_fence("objective", &payload()),
                if_doc_rev: 0
            },
            EditAuthor::Planner
        )
        .is_err()
    );
}

#[test]
fn objective_moves_preserve_blocks_and_markers_are_refused() {
    let mut original = doc();
    let before = original.blocks_snapshot().unwrap()[0].clone();
    apply_report_op(
        &mut original,
        &ReportDocOp::MoveBlock {
            id: before.id.clone(),
            to_index: 0,
            if_doc_rev: 0,
        },
        EditAuthor::Assistant,
    )
    .unwrap();
    assert_eq!(original.blocks_snapshot().unwrap()[0], before);
    let error = apply_report_op(
        &mut doc(),
        &ReportDocOp::WriteMarkdown {
            summary: None,
            body: format!("<!-- neige:{} -->\n# Changed\n", before.id),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(matches!(error,CalmError::BadRequest(reason) if reason.contains("objective block")));
}
