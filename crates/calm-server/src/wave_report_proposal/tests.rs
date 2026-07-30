//! Unit tests for the #955 §5.2.1 proposal-op lowering. Split out of
//! `mod.rs` to keep both files under the 800-line ceiling; every test
//! here exercises `super`'s public surface only.

use super::*;
use calm_types::wave_report::WaveReportPayload;
use serde_json::json;

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
    assert_eq!(blocks.len(), 2);
    (doc, blocks)
}

fn prose(markdown: &str) -> serde_json::Value {
    json!({ "markdown": markdown })
}

fn create(temp_id: &str, anchor: ProposalAnchor) -> ProposalOp {
    ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: Some(temp_id.into()),
        kind: "prose".into(),
        payload: prose(&format!("# {temp_id}\n\nbody of {temp_id}\n")),
        if_rev: None,
        anchor: Some(anchor),
    }
}

fn body_order(doc: &ReportDoc) -> Vec<String> {
    doc.block_index()
        .unwrap()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect()
}

// ----- structural validation -------------------------------------------

#[test]
fn validate_rejects_empty_ops() {
    assert!(validate_proposal_ops(&[]).is_err());
}

#[test]
fn validate_requires_if_rev_on_replace_move_delete() {
    let replace_without_rev = ProposalOp::UpsertBlock {
        block_id: Some("b_0001".into()),
        temp_id: None,
        kind: "prose".into(),
        payload: prose("x\n"),
        if_rev: None,
        anchor: None,
    };
    let err = validate_proposal_ops(&[replace_without_rev]).unwrap_err();
    assert!(err.contains("if_rev"), "{err}");
    // move/delete carry `if_rev` as a required wire field, so their
    // mandatory-ness is a type-level guarantee, not a runtime check.
}

#[test]
fn validate_rejects_both_and_neither_id() {
    let both = ProposalOp::UpsertBlock {
        block_id: Some("b_0001".into()),
        temp_id: Some("t".into()),
        kind: "prose".into(),
        payload: prose("x\n"),
        if_rev: Some(1),
        anchor: None,
    };
    assert!(validate_proposal_ops(&[both]).unwrap_err().contains("both"));
    let neither = ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: None,
        kind: "prose".into(),
        payload: prose("x\n"),
        if_rev: None,
        anchor: None,
    };
    assert!(validate_proposal_ops(&[neither]).is_err());
}

#[test]
fn validate_requires_anchor_on_create_and_forbids_it_on_replace() {
    let create_without_anchor = ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: Some("t".into()),
        kind: "prose".into(),
        payload: prose("x\n"),
        if_rev: None,
        anchor: None,
    };
    assert!(
        validate_proposal_ops(&[create_without_anchor])
            .unwrap_err()
            .contains("anchor")
    );
    let replace_with_anchor = ProposalOp::UpsertBlock {
        block_id: Some("b_0001".into()),
        temp_id: None,
        kind: "prose".into(),
        payload: prose("x\n"),
        if_rev: Some(1),
        anchor: Some(ProposalAnchor::AtEnd),
    };
    assert!(
        validate_proposal_ops(&[replace_with_anchor])
            .unwrap_err()
            .contains("anchor")
    );
}

#[test]
fn validate_rejects_forward_and_unknown_temp_refs() {
    // Forward reference: op 0 anchors on a temp id op 1 declares.
    let ops = vec![
        create("t2_ref", ProposalAnchor::AfterBlockId("temp:t2".into())),
        create("t2", ProposalAnchor::AtEnd),
    ];
    let err = validate_proposal_ops(&ops).unwrap_err();
    assert!(err.contains("temp_id"), "{err}");
    // Backward reference resolves.
    let ops = vec![
        create("t2", ProposalAnchor::AtEnd),
        create("t3", ProposalAnchor::AfterBlockId("temp:t2".into())),
    ];
    validate_proposal_ops(&ops).expect("backward temp ref is legal");
}

#[test]
fn validate_rejects_duplicate_temp_ids() {
    let ops = vec![
        create("t", ProposalAnchor::AtEnd),
        create("t", ProposalAnchor::AtEnd),
    ];
    assert!(
        validate_proposal_ops(&ops)
            .unwrap_err()
            .contains("duplicate")
    );
}

#[test]
fn validate_rejects_unknown_kind_and_bad_payload() {
    let bad_kind = ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: Some("t".into()),
        kind: "not-a-kind".into(),
        payload: json!({}),
        if_rev: None,
        anchor: Some(ProposalAnchor::AtEnd),
    };
    assert!(
        validate_proposal_ops(&[bad_kind])
            .unwrap_err()
            .contains("unknown block kind")
    );
    let prose_without_markdown = ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: Some("t".into()),
        kind: "prose".into(),
        payload: json!({ "text": "oops" }),
        if_rev: None,
        anchor: Some(ProposalAnchor::AtEnd),
    };
    assert!(
        validate_proposal_ops(&[prose_without_markdown])
            .unwrap_err()
            .contains("markdown")
    );
}

#[test]
fn validate_rejects_prose_smuggling_a_data_fence() {
    let smuggler = ProposalOp::UpsertBlock {
        block_id: None,
        temp_id: Some("t".into()),
        kind: "prose".into(),
        payload: prose("```neige-block table\n{}\n```\n"),
        if_rev: None,
        anchor: Some(ProposalAnchor::AtEnd),
    };
    assert!(validate_proposal_ops(&[smuggler]).is_err());
}

#[test]
fn validate_rejects_self_anchored_move() {
    let op = ProposalOp::MoveBlock {
        block_id: "b_0001".into(),
        if_rev: 1,
        anchor: ProposalAnchor::AfterBlockId("b_0001".into()),
    };
    assert!(
        validate_proposal_ops(&[op])
            .unwrap_err()
            .contains("after itself")
    );
}

// ----- apply / lowering -------------------------------------------------

#[test]
fn temp_id_creation_mints_a_kernel_id_and_later_anchors_resolve() {
    let (mut doc, blocks) = two_block_doc();
    let heads = doc.doc_heads();
    let (a_id, _) = blocks[0].clone();
    // t1 lands right after block A; t2 lands right after t1.
    let ops = vec![
        create("t1", ProposalAnchor::AfterBlockId(a_id.clone())),
        create("t2", ProposalAnchor::AfterBlockId("temp:t1".into())),
    ];
    validate_proposal_ops(&ops).expect("structurally valid");
    apply_proposal_batch(&mut doc, &heads, &ops).expect("batch applies");
    let order = body_order(&doc);
    assert_eq!(order.len(), 4, "two blocks created: {order:?}");
    assert_eq!(order[0], a_id);
    // The two new ids are kernel-assigned `b_xxxx` values, not the
    // proposal's temp ids.
    assert!(
        order[1..3].iter().all(|id| id.starts_with("b_")),
        "kernel must mint durable ids: {order:?}"
    );
    assert!(
        !order
            .iter()
            .any(|id| id.contains("t1") || id.contains("t2")),
        "temp ids must not leak into storage: {order:?}"
    );
    let (_, body) = doc.project().unwrap();
    let t1_at = body.find("body of t1").expect("t1 body present");
    let t2_at = body.find("body of t2").expect("t2 body present");
    assert!(t1_at < t2_at, "t2 must follow t1: {body}");
}

#[test]
fn anchors_resolve_to_positions_not_indexes() {
    let (mut doc, blocks) = two_block_doc();
    let heads = doc.doc_heads();
    let (a_id, _) = blocks[0].clone();
    let (b_id, b_rev) = blocks[1].clone();
    // at_start creation + a move of B to the very front, in one batch.
    let ops = vec![
        create("head", ProposalAnchor::AtStart),
        ProposalOp::MoveBlock {
            block_id: b_id.clone(),
            if_rev: b_rev,
            anchor: ProposalAnchor::AtStart,
        },
    ];
    apply_proposal_batch(&mut doc, &heads, &ops).expect("batch applies");
    let order = body_order(&doc);
    assert_eq!(order.len(), 3);
    assert_eq!(order[0], b_id, "B moved to the head: {order:?}");
    assert_eq!(order[2], a_id, "A stays last: {order:?}");
}

#[test]
fn move_after_block_lands_directly_after_it() {
    // Three-block doc so "after the middle block" is unambiguous.
    let mut doc = ReportDoc::from_payload(&WaveReportPayload::new(
        "s",
        "# A\n\nalpha\n\n# B\n\nbeta\n\n# C\n\ngamma\n",
    ));
    let snapshot = doc.blocks_snapshot().unwrap();
    let (a_id, a_rev) = (snapshot[0].id.clone(), snapshot[0].rev);
    let (b_id, _) = (snapshot[1].id.clone(), snapshot[1].rev);
    let (c_id, c_rev) = (snapshot[2].id.clone(), snapshot[2].rev);
    let heads = doc.doc_heads();
    // Move A (index 0) after B (index 1) → order B, A, C.
    apply_proposal_batch(
        &mut doc,
        &heads,
        &[ProposalOp::MoveBlock {
            block_id: a_id.clone(),
            if_rev: a_rev,
            anchor: ProposalAnchor::AfterBlockId(b_id.clone()),
        }],
    )
    .expect("forward move applies");
    assert_eq!(
        body_order(&doc),
        vec![b_id.clone(), a_id.clone(), c_id.clone()]
    );
    // Move C (now index 2) after B (index 0) → order B, C, A.
    let heads = doc.doc_heads();
    apply_proposal_batch(
        &mut doc,
        &heads,
        &[ProposalOp::MoveBlock {
            block_id: c_id.clone(),
            if_rev: c_rev,
            anchor: ProposalAnchor::AfterBlockId(b_id.clone()),
        }],
    )
    .expect("backward move applies");
    assert_eq!(body_order(&doc), vec![b_id, c_id, a_id]);
}

#[test]
fn stale_if_rev_is_the_conflict_class_and_rolls_back() {
    let (mut doc, blocks) = two_block_doc();
    let heads = doc.doc_heads();
    let (a_id, a_rev) = blocks[0].clone();
    let before = doc.project().unwrap();
    let err = apply_proposal_batch(
        &mut doc,
        &heads,
        &[
            create("t", ProposalAnchor::AfterBlockId(a_id.clone())),
            ProposalOp::DeleteBlock {
                block_id: a_id,
                if_rev: a_rev + 7,
            },
        ],
    )
    .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)), "got {err:?}");
    assert!(format!("{err:?}").contains("ops[1]"), "op index: {err:?}");
    assert_eq!(doc.project().unwrap(), before, "whole batch rolled back");
}

#[test]
fn unknown_block_id_is_conflict_unknown_temp_ref_is_bad_request() {
    let (mut doc, _) = two_block_doc();
    let heads = doc.doc_heads();
    // Vanished anchor target ⇒ staleness.
    let gone = apply_proposal_batch(
        &mut doc,
        &heads,
        &[create("t", ProposalAnchor::AfterBlockId("b_gone".into()))],
    )
    .unwrap_err();
    assert!(matches!(gone, CalmError::Conflict(_)), "got {gone:?}");
    // A `temp:` reference this proposal never creates is structural —
    // no fresh snapshot can fix it, so it must NOT read as stale.
    let bogus_temp = apply_proposal_batch(
        &mut doc,
        &heads,
        &[create(
            "t",
            ProposalAnchor::AfterBlockId("temp:nope".into()),
        )],
    )
    .unwrap_err();
    assert!(
        matches!(bogus_temp, CalmError::BadRequest(_)),
        "got {bogus_temp:?}"
    );
}

#[test]
fn base_doc_heads_gates_only_document_anchored_creations() {
    let (mut doc, blocks) = two_block_doc();
    let (a_id, a_rev) = blocks[0].clone();
    let stale_heads = "ah1:deadbeef";
    // at_end creation with a moved baseline ⇒ stale.
    let err = apply_proposal_batch(&mut doc, stale_heads, &[create("t", ProposalAnchor::AtEnd)])
        .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)), "got {err:?}");
    // A block-anchored op with the SAME moved baseline is unaffected:
    // its `if_rev` carries the authority, which is what lets a
    // proposal survive edits to unrelated blocks (§5.2.1).
    apply_proposal_batch(
        &mut doc,
        stale_heads,
        &[ProposalOp::DeleteBlock {
            block_id: a_id,
            if_rev: a_rev,
        }],
    )
    .expect("block-anchored op ignores base_doc_heads");
}

#[test]
fn empty_batch_is_bad_request() {
    let (mut doc, _) = two_block_doc();
    let heads = doc.doc_heads();
    let err = apply_proposal_batch(&mut doc, &heads, &[]).unwrap_err();
    assert!(matches!(err, CalmError::BadRequest(_)), "got {err:?}");
}

#[test]
fn replace_renders_data_kind_payload_to_its_canonical_fence() {
    let (mut doc, blocks) = two_block_doc();
    let heads = doc.doc_heads();
    let (a_id, a_rev) = blocks[0].clone();
    let payload = json!({
        "columns": [{ "key": "k", "label": "K" }],
        "rows": [{ "k": "v" }],
    });
    let ops = vec![ProposalOp::UpsertBlock {
        block_id: Some(a_id.clone()),
        temp_id: None,
        kind: "table".into(),
        payload: payload.clone(),
        if_rev: Some(a_rev),
        anchor: None,
    }];
    validate_proposal_ops(&ops).expect("table payload validates");
    apply_proposal_batch(&mut doc, &heads, &ops).expect("replace applies");
    let (_, body) = doc.project().unwrap();
    assert!(
        body.contains("```neige-block") && body.contains("\"k\""),
        "canonical fence must be stored: {body}"
    );
}
