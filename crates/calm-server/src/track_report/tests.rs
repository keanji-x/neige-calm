use super::*;
use serde_json::json;

#[test]
fn whole_tree_total_postcondition_rejects_an_over_budget_inventory() {
    // Feed a deliberately inconsistent share map to prove the independent fail-closed guard
    // remains live even if the share construction is corrupted.
    let shares = std::collections::BTreeMap::from([("root".to_owned(), 9)]);
    let error = require_tree_budget_postcondition("root", 8, &shares, &[("root".to_owned(), 9)])
        .unwrap_err();
    assert!(
        matches!(error, CalmError::Conflict(message) if message.contains("9 unfinished planner task(s)") && message.contains("tree_task_budget of 8"))
    );
}

/// Both directions, because a constructor that rejected everything would pass the first assertion on its own.
#[test]
fn report_edit_target_pairs_a_card_only_with_its_own_owner() {
    fn parts(card_owner: &str) -> (Track, Card) {
        let owner = serde_json::from_value(json!({
            "id": "w_a", "area_id": "a_1", "title": "A", "sort": 1.0,
            "pinned_at": null, "cwd": "",
            "created_at": 0, "updated_at": 0
        }))
        .expect("owner fixture");
        let card = serde_json::from_value(json!({
            "id": "c_1", "track_id": card_owner, "kind": "track-report",
            "sort": 1.0, "payload": {}, "created_at": 0, "updated_at": 0
        }))
        .expect("card fixture");
        (owner, card)
    }

    let (owner, own_card) = parts("w_a");
    ReportEditTarget::for_resolved_parts(owner, own_card, TrackReportPayload::initial())
        .expect("its own report card must build a target");

    let (owner, foreign_card) = parts("w_b");
    // `let ... else` rather than `expect_err`: the latter would need `ReportEditTarget: Debug`.
    let Err(error) =
        ReportEditTarget::for_resolved_parts(owner, foreign_card, TrackReportPayload::initial())
    else {
        panic!("a report card from another owner must not build a target");
    };
    assert!(
        matches!(&error, CalmError::Internal(message)
                if message.contains("belongs to track w_b, not w_a")),
        "the error must name both so the mismatch is diagnosable, got: {error:?}"
    );
}

#[test]
fn initial_carries_current_schema_version() {
    let p = TrackReportPayload::initial();
    assert_eq!(p.schema_version, TrackReportPayload::SCHEMA_VERSION);
    assert!(p.summary.is_empty());
    assert!(p.body.contains("# 概要"));
    assert!(p.body.ends_with('\n'));
}

#[test]
fn serde_round_trip_camelcase_wire() {
    let p = TrackReportPayload::new("hi", "# A\n\nb\n");
    let v = serde_json::to_value(&p).unwrap();
    // Wire shape: camelCase keys. A drift here would break the frontend's zod schema silently.
    assert_eq!(
        v,
        json!({
            "schemaVersion": 4,
            "docRev": 0,
            "summary": "hi",
            "body": "# A\n\nb\n",
        })
    );
    let back: TrackReportPayload = serde_json::from_value(v).unwrap();
    assert_eq!(back, p);
}

#[test]
fn deserialize_rejects_missing_fields() {
    // No `body`.
    let err = serde_json::from_value::<TrackReportPayload>(json!({
        "schemaVersion": 1,
        "summary": "x"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("body"), "got: {err}");

    // No `summary`.
    let err = serde_json::from_value::<TrackReportPayload>(json!({
        "schemaVersion": 1,
        "body": "x"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("summary"), "got: {err}");
}

#[test]
fn apply_op_with_none_summary_resolves_from_doc_inside_tx() {
    // `summary: None` must resolve against the doc — the in-tx truth — not any caller-side
    // snapshot. Simulate the race by moving the doc's summary after "the caller read it".
    let mut doc =
        ReportDoc::from_payload(&TrackReportPayload::new("stale snapshot", "# A\n\nalpha\n"));
    doc.update("racing summary", "# A\n\nalpha\n").unwrap();

    let outcome = apply_report_op(
        &mut doc,
        &ReportDocOp::WriteMarkdown {
            summary: None,
            body: "# A\n\nalpha edited\n".into(),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .unwrap();
    assert!(outcome.is_none());
    let (summary, body) = doc.project().unwrap();
    assert_eq!(
        summary, "racing summary",
        "None must keep the doc's current (in-tx) summary"
    );
    assert_eq!(body, "# A\n\nalpha edited\n");

    // The user's Replace always names its summary.
    apply_report_op(
        &mut doc,
        &ReportDocOp::Replace {
            summary: "explicit".into(),
            body: "# C\n\ngamma\n".into(),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .unwrap();
    assert_eq!(doc.project().unwrap().0, "explicit");
}

#[test]
fn every_report_doc_op_advances_document_revision() {
    fn assert_advances(mut doc: ReportDoc, op: ReportDocOp) {
        let before = doc.doc_rev().unwrap();
        apply_persisted_report_op(&mut doc, &op, EditAuthor::Planner).unwrap();
        assert_eq!(doc.doc_rev().unwrap(), before + 1, "op: {op:?}");
    }

    let payload = TrackReportPayload::new("summary", "# A\n\nalpha\n\n# B\n\nbeta\n");
    let base = ReportDoc::from_payload(&payload);
    let blocks = base.block_index().unwrap();
    let first = blocks[0].clone();
    let second = blocks[1].clone();

    // Content-equal replace is deliberately included: it is a document write even when no block revision changes.
    assert_advances(
        ReportDoc::from_payload(&payload),
        ReportDocOp::Replace {
            summary: payload.summary.clone(),
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
    // Reset is the user's alone, so it is asserted under that author.
    let mut doc = ReportDoc::from_payload(&payload);
    apply_persisted_report_op(
        &mut doc,
        &ReportDocOp::ResetToInitial { if_doc_rev: 0 },
        EditAuthor::User,
    )
    .unwrap();
    assert_eq!(doc.doc_rev().unwrap(), 1, "op: ResetToInitial");
}

#[test]
fn apply_op_on_malformed_doc_is_internal_not_bad_request() {
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjType, ROOT};

    // Shape 1: block rev stored as a Str. Must surface Internal, never "block not found".
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
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");

    // Shape 2: blocks map present but no order list. The corrupt doc must not be read as an empty
    // report and silently overwritten.
    let mut raw = AutoCommit::new();
    let summary_id = raw.put_object(&ROOT, "summary", ObjType::Text).unwrap();
    raw.update_text(&summary_id, "s").unwrap();
    raw.put_object(&ROOT, "blocks", ObjType::Map).unwrap();
    let mut doc = ReportDoc::from_bytes(&raw.save()).unwrap();
    let err = apply_report_op(
        &mut doc,
        &ReportDocOp::Replace {
            summary: "s".into(),
            body: String::new(),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");
}

// The contract-header ingress rules at the op layer; where a header may SIT is the funnel's
// question and lives with the persist-path tests.

use calm_types::report_contract::{ContractHeader, ContractSection, HEADER_OPEN, canonical_line};

/// A one-section header as a caller might spell it: keys out of declaration order and an
/// explicit `"omit_if_empty":false`, both of which the canonical form drops.
const NON_CANONICAL_HEADER: &str = "<!-- neige:contract {\"sections\":[{\"omit_if_empty\":false,\"h1\":\"概要\"}],\"version\":1} -->";

fn one_section_header() -> ContractHeader {
    ContractHeader {
        version: 1,
        sections: vec![ContractSection {
            h1: "概要".into(),
            omit_if_empty: false,
        }],
    }
}

fn first_line(body: &str) -> &str {
    body.split('\n').next().unwrap_or_default()
}

#[test]
fn replace_normalizes_a_non_canonical_header_line() {
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let rest = "\n\n# 概要\n\nwritten\n";
    assert_ne!(
        NON_CANONICAL_HEADER,
        canonical_line(&one_section_header()),
        "the fixture must be non-canonical or this test proves nothing"
    );
    apply_report_op(
        &mut doc,
        &ReportDocOp::Replace {
            summary: "s".into(),
            body: format!("{NON_CANONICAL_HEADER}{rest}"),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .expect("a non-canonical header is normalized, not refused");
    let (_, body) = doc.project().unwrap();
    assert_eq!(first_line(&body), canonical_line(&one_section_header()));
    assert_eq!(
        &body[first_line(&body).len()..],
        rest,
        "every byte after line 1 is untouched"
    );
}

#[test]
fn replace_with_a_malformed_header_is_bad_request_and_leaves_the_doc_unchanged() {
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let before = doc.project().unwrap();
    let err = apply_persisted_report_op(
        &mut doc,
        &ReportDocOp::Replace {
            summary: "s".into(),
            body: format!(
                "{HEADER_OPEN}{{\"version\":1,\"sections\":[{{\"h1\":\"-->\"}}]}} -->\n\n# A\n"
            ),
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(
        matches!(&err, CalmError::BadRequest(m) if m.starts_with("report contract header: ")),
        "got {err:?}"
    );
    assert_eq!(
        doc.project().unwrap(),
        before,
        "a refused write lands nothing"
    );
    assert_eq!(doc.doc_rev().unwrap(), 0, "and advances nothing");
}

/// A `with_markers` read puts the marker on line 1 and the header on line 2, so normalizing
/// before the strip would see a marker, not a header.
#[test]
fn write_markdown_normalizes_the_header_after_stripping_markers() {
    let canonical = canonical_line(&one_section_header());
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new(
        "s",
        format!("{canonical}\n\n# 概要\n\nalpha\n"),
    ));
    let index = doc.block_index().unwrap();
    assert_eq!(index.len(), 2, "contract block + one section");
    let (contract_id, section_id) = (index[0].0.clone(), index[1].0.clone());

    let marked = format!(
        "<!-- neige:{contract_id} -->\n{NON_CANONICAL_HEADER}\n\n<!-- neige:{section_id} -->\n# 概要\n\nalpha edited\n"
    );
    apply_report_op(
        &mut doc,
        &ReportDocOp::WriteMarkdown {
            summary: None,
            body: marked,
            if_doc_rev: 0,
        },
        EditAuthor::Planner,
    )
    .expect("write_markdown with markers and a non-canonical header");
    let (_, body) = doc.project().unwrap();
    assert_eq!(
        first_line(&body),
        canonical,
        "line 1 canonical after the strip"
    );
    assert!(
        !body.contains("<!-- neige:b_"),
        "markers never reach storage: {body:?}"
    );
    let after = doc.block_index().unwrap();
    assert_eq!(
        after
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>(),
        [contract_id.as_str(), section_id.as_str()],
        "the rebuilt slices were paired with the original hints"
    );
    assert_eq!(after[1].2, 2, "the edited section bumped its rev");
}

/// The doc here has no header, so the resulting document is one the funnel accepts; the
/// "doc already had one" half is a persist-path test.
#[test]
fn upsert_prose_at_position_0_carrying_a_header_is_normalized() {
    let canonical = canonical_line(&one_section_header());

    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    apply_report_op(
        &mut doc,
        &ReportDocOp::UpsertBlock {
            id: None,
            kind: KIND_PROSE.into(),
            content: format!("{NON_CANONICAL_HEADER}\n"),
            if_rev: None,
            if_doc_rev: Some(0),
            position: Some(0),
        },
        EditAuthor::Planner,
    )
    .expect("single upsert");
    let (_, body) = doc.project().unwrap();
    assert_eq!(first_line(&body), canonical, "single op: {body:?}");

    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    apply_report_op(
        &mut doc,
        &ReportDocOp::Batch {
            doc_anchor: DocAnchor::LastRead(0),
            summary: None,
            ops: vec![BatchBlockOp::Upsert {
                id: None,
                kind: KIND_PROSE.into(),
                content: format!("{NON_CANONICAL_HEADER}\n"),
                if_rev: None,
                position: Some(0),
            }],
        },
        EditAuthor::Planner,
    )
    .expect("batch upsert");
    let (_, body) = doc.project().unwrap();
    assert_eq!(first_line(&body), canonical, "batch step: {body:?}");

    // A malformed header is refused at the same ingress, with the step index the batch prefixes.
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let err = apply_report_op(
        &mut doc,
        &ReportDocOp::Batch {
            doc_anchor: DocAnchor::LastRead(0),
            summary: None,
            ops: vec![BatchBlockOp::Upsert {
                id: None,
                kind: KIND_PROSE.into(),
                content: format!("{HEADER_OPEN}not json -->\n"),
                if_rev: None,
                position: Some(0),
            }],
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(
        matches!(&err, CalmError::BadRequest(m)
                if m.starts_with("ops[0]: report contract header: ")),
        "got {err:?}"
    );
}

/// A stale anchor is judged before the content, so stale `if_rev` / `if_doc_rev` plus a
/// malformed header is still a `Conflict`.
#[test]
fn a_stale_rev_beside_a_malformed_header_is_still_a_conflict() {
    let malformed = format!("{HEADER_OPEN}not json -->\n");
    let payload = TrackReportPayload::new("s", "# A\n\nalpha\n");
    let mut doc = ReportDoc::from_payload(&payload);
    let (id, _, rev) = doc.block_index().unwrap()[0].clone();

    let err = apply_report_op(
        &mut doc,
        &ReportDocOp::UpsertBlock {
            id: Some(id.clone()),
            kind: KIND_PROSE.into(),
            content: malformed.clone(),
            if_rev: Some(rev + 1),
            if_doc_rev: None,
            position: None,
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(
        matches!(err, CalmError::Conflict(_)),
        "replace arm: {err:?}"
    );

    let err = apply_report_op(
        &mut doc,
        &ReportDocOp::UpsertBlock {
            id: None,
            kind: KIND_PROSE.into(),
            content: malformed.clone(),
            if_rev: None,
            if_doc_rev: Some(7),
            position: Some(0),
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)), "create arm: {err:?}");

    let err = apply_report_op(
        &mut doc,
        &ReportDocOp::Batch {
            doc_anchor: DocAnchor::LastRead(0),
            summary: None,
            ops: vec![BatchBlockOp::Upsert {
                id: Some(id),
                kind: KIND_PROSE.into(),
                content: malformed,
                if_rev: Some(rev + 1),
                position: None,
            }],
        },
        EditAuthor::Planner,
    )
    .unwrap_err();
    assert!(
        matches!(&err, CalmError::Conflict(m) if m.starts_with("ops[0]: ")),
        "batch step: {err:?}"
    );
    assert_eq!(
        doc.project().unwrap(),
        ("s".to_string(), "# A\n\nalpha\n".to_string())
    );
}

#[test]
fn report_snapshot_with_crdt_reads_the_stored_projection_and_requires_it() {
    let mut payload = TrackReportPayload::new("stored", "# Body\n\nnot the blocks\n");
    let blocks = vec![ReportBlock {
        id: "b_0001".into(),
        kind: "prose".into(),
        rev: 3,
        payload: json!({"markdown": "stored block"}),
    }];
    payload.blocks = Some(blocks.clone());
    let row = |payload: &TrackReportPayload| Some((serde_json::to_string(payload).unwrap(), true));
    assert_eq!(
        report_blocks_snapshot_from_row("t", row(&payload)).unwrap(),
        ("stored".to_string(), blocks)
    );
    payload.blocks = None;
    let error = report_blocks_snapshot_from_row("t", row(&payload)).unwrap_err();
    assert!(
        matches!(&error, CalmError::Internal(m) if m.contains("no stored block projection")),
        "{error:?}"
    );
}
