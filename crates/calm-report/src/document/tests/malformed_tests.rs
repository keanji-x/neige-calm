use super::*;

fn raw_doc_with_summary(summary: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let summary_id = doc.put_object(&ROOT, FIELD_SUMMARY, ObjType::Text).unwrap();
    doc.update_text(&summary_id, summary).unwrap();
    doc
}

#[test]
fn malformed_dangling_order_id_errors_instead_of_panicking() {
    let mut raw = raw_doc_with_summary("s");
    raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_dead").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.project().unwrap_err();
    assert!(err.to_string().contains("no blocks entry"), "err = {err:#}");
    assert!(doc.blocks_snapshot().is_err());
    assert!(doc.block_index().is_err());
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(doc.update("s", "# A\n").is_err());
}

#[test]
fn malformed_non_text_block_field_errors_instead_of_panicking() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "prose").unwrap();
    raw.put(&entry, KEY_REV, 1_u64).unwrap();
    raw.put(&entry, KEY_TEXT, "scalar, not Text").unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.project().unwrap_err();
    assert!(
        format!("{err:#}").contains("not a Text object"),
        "err = {err:#}"
    );
    assert!(doc.blocks_snapshot().is_err());
}

#[test]
fn malformed_missing_summary_errors_instead_of_panicking() {
    let mut raw = AutoCommit::new();
    raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.project().unwrap_err();
    assert!(
        format!("{err:#}").contains("missing `summary`"),
        "err = {err:#}"
    );
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(doc.update("s", "# A\n").is_err(), "update must not panic");
    assert!(
        doc.update_with_hints("s", &split_body("# A\n"), &[])
            .is_err(),
        "update_with_hints must not panic"
    );
}

fn raw_doc_blocks_without_order() -> Vec<u8> {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "prose").unwrap();
    raw.put(&entry, KEY_REV, 1_u64).unwrap();
    let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
    raw.update_text(&text_id, "# A\n").unwrap();
    raw.save()
}

#[test]
fn blocks_without_order_is_corruption_not_an_empty_report() {
    let bytes = raw_doc_blocks_without_order();
    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.project().unwrap_err();
    assert!(
        format!("{err:#}").contains("order list missing"),
        "err = {err:#}"
    );
    assert!(doc.blocks_snapshot().is_err());
    assert!(doc.has_blocks_layout().is_err());
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(
        doc.update("s", "").is_err(),
        "an empty-body write over the corrupt doc must be refused"
    );
}

#[test]
fn scalar_order_is_corruption_not_an_empty_report() {
    let mut raw = raw_doc_with_summary("s");
    raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    raw.put(&ROOT, FIELD_ORDER, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.project().unwrap_err();
    assert!(format!("{err:#}").contains("not a List"), "err = {err:#}");
    assert!(doc.blocks_snapshot().is_err());
    assert!(doc.has_blocks_layout().is_err());
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(doc.update("s", "# A\n").is_err());
}

#[test]
fn scalar_rev_is_corruption_not_block_not_found() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "prose").unwrap();
    raw.put(&entry, KEY_REV, "three").unwrap();
    let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
    raw.update_text(&text_id, "# A\n").unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.blocks_snapshot().unwrap_err();
    assert!(format!("{err:#}").contains("no Uint rev"), "err = {err:#}");
    assert!(doc.has_blocks_layout().is_err());
    assert!(
        doc.block_rev("b_0001").is_err(),
        "rev corruption must be an error, not Ok(None)"
    );
    assert_eq!(doc.block_rev("b_nope").unwrap(), None);
}

#[test]
fn out_of_range_rev_is_corruption_not_saturation() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "prose").unwrap();
    raw.put(&entry, KEY_REV, u64::from(u32::MAX) + 1).unwrap();
    let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
    raw.update_text(&text_id, "# A\n").unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.blocks_snapshot().unwrap_err();
    assert!(format!("{err:#}").contains("exceeds u32"), "err = {err:#}");
    assert!(doc.block_rev("b_0001").is_err());
    assert!(doc.has_blocks_layout().is_err());
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(
        doc.upsert_block(Some("b_0001"), "prose", "x\n").is_err(),
        "replace over a corrupt rev must be refused"
    );
}

#[test]
fn duplicate_order_id_is_corruption() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "prose").unwrap();
    raw.put(&entry, KEY_REV, 1_u64).unwrap();
    let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
    raw.update_text(&text_id, "# A\n").unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    raw.insert(&order, 1, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.blocks_snapshot().unwrap_err();
    assert!(
        format!("{err:#}").contains("duplicate id b_0001 in order"),
        "err = {err:#}"
    );
    assert!(doc.has_blocks_layout().is_err());
}

#[test]
fn hidden_blocks_entry_outside_order_is_corruption() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    for id in ["b_0001", "b_hidden"] {
        let entry = raw.put_object(&blocks, id, ObjType::Map).unwrap();
        raw.put(&entry, KEY_KIND, "prose").unwrap();
        raw.put(&entry, KEY_REV, 1_u64).unwrap();
        let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
    }
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.blocks_snapshot().unwrap_err();
    assert!(
        format!("{err:#}").contains("blocks map has 2 entries but order lists 1"),
        "err = {err:#}"
    );
    assert!(doc.has_blocks_layout().is_err());
}

#[test]
fn duplicate_hint_ids_migrate_with_unique_order() {
    let body = "# A\n\nalpha\n\n# B\n\nbeta\n";
    let hint = vec![
        ReportBlock {
            id: "b_dupe".to_string(),
            kind: "prose".to_string(),
            rev: 2,
            payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
        },
        ReportBlock {
            id: "b_dupe".to_string(),
            kind: "prose".to_string(),
            rev: 5,
            payload: json!({ "markdown": "# B\n\nbeta\n" }),
        },
    ];
    let bytes = legacy_doc_bytes("s", body);
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(doc.ensure_blocks_layout(Some(&hint)).unwrap());
    assert_eq!(
        doc.project().unwrap(),
        ("s".to_string(), body.to_string()),
        "projection is byte-identical"
    );
    let ids: Vec<String> = doc
        .block_index()
        .unwrap()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "b_dupe", "first occurrence keeps the id");
    assert_ne!(ids[1], "b_dupe", "duplicate gets a fresh id");
    assert_eq!(
        ids.iter().collect::<HashSet<_>>().len(),
        ids.len(),
        "order ids are unique"
    );
}

#[test]
fn upsert_non_prose_block_stores_canonical_fence_and_snapshot_parses_it() {
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let payload = json!({ "src": "/apps/x", "height": 480 });
    let fence_text = calm_types::report_blocks::render_fence("app", &payload);
    let (id, rev) = doc.upsert_block(None, "app", &fence_text).unwrap();
    assert_eq!(rev, 1);

    let (_, body) = doc.project().unwrap();
    assert_eq!(body, format!("# A\n\nalpha\n{fence_text}"));
    let blocks = doc.blocks_snapshot().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1].id, id);
    assert_eq!(blocks[1].kind, "app");
    assert_eq!(blocks[1].payload, payload);
    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).unwrap();
    assert_eq!(reloaded.project().unwrap().1, body);
    assert_eq!(reloaded.blocks_snapshot().unwrap()[1].payload, payload);

    let (_, rev) = doc.upsert_block(Some(&id), "app", &fence_text).unwrap();
    assert_eq!(rev, 1, "identical fence: rev holds");
    let changed = calm_types::report_blocks::render_fence("app", &json!({ "src": "/apps/y" }));
    let (_, rev) = doc.upsert_block(Some(&id), "app", &changed).unwrap();
    assert_eq!(rev, 2, "changed payload: rev+1");

    assert!(doc.upsert_block(Some(&id), "app", "not a fence\n").is_err());
    assert!(doc.upsert_block(Some(&id), "table", &changed).is_err());
}

#[test]
fn non_prose_text_that_is_not_a_fence_is_corruption() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = raw.put_object(&ROOT, FIELD_BLOCKS, ObjType::Map).unwrap();
    let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
    raw.put(&entry, KEY_KIND, "chart.candles").unwrap();
    raw.put(&entry, KEY_REV, 1_u64).unwrap();
    let text_id = raw.put_object(&entry, KEY_TEXT, ObjType::Text).unwrap();
    raw.update_text(&text_id, "just markdown, no fence\n")
        .unwrap();
    let order = raw.put_object(&ROOT, FIELD_ORDER, ObjType::List).unwrap();
    raw.insert(&order, 0, "b_0001").unwrap();
    let bytes = raw.save();

    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    let err = doc.blocks_snapshot().unwrap_err();
    assert!(
        format!("{err:#}").contains("not a well-formed neige-block fence"),
        "err = {err:#}"
    );
    assert!(doc.has_blocks_layout().is_err());
    assert!(doc.project().is_ok());
}

#[test]
fn wholesale_update_carrying_the_fence_verbatim_preserves_the_block() {
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let payload = json!({ "src": "/apps/x" });
    let fence_text = calm_types::report_blocks::render_fence("app", &payload);
    let (id, _) = doc.upsert_block(None, "app", &fence_text).unwrap();

    doc.update("s", &format!("# A\n\nalpha edited\n{fence_text}"))
        .unwrap();
    let blocks = doc.blocks_snapshot().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].rev, 2, "edited prose: rev+1");
    assert_eq!(blocks[1].id, id);
    assert_eq!(blocks[1].kind, "app");
    assert_eq!(blocks[1].rev, 1, "untouched fence: rev holds");
    assert_eq!(blocks[1].payload, payload);
}

#[test]
fn write_blocks_layout_dedupes_duplicate_ids_defensively() {
    let mut raw = raw_doc_with_summary("s");
    let blocks = vec![
        ReportBlock {
            id: "b_dupe".to_string(),
            kind: "prose".to_string(),
            rev: 1,
            payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
        },
        ReportBlock {
            id: "b_dupe".to_string(),
            kind: "prose".to_string(),
            rev: 1,
            payload: json!({ "markdown": "# B\n\nbeta\n" }),
        },
    ];
    ReportDoc::write_blocks_layout(&mut raw, &blocks);
    let doc = ReportDoc(raw);
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha\n\n# B\n\nbeta\n");
    let ids: Vec<String> = doc
        .block_index()
        .unwrap()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(ids[0], "b_dupe");
    assert_ne!(ids[1], "b_dupe");
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
}

#[test]
fn from_blocks_exact_preserves_order_ids_revs_and_payloads() {
    let blocks = vec![
        ReportBlock {
            id: "b_0001".into(),
            kind: "prose".into(),
            rev: 7,
            payload: json!({ "markdown": "# A\n\nalpha\n\n" }),
        },
        ReportBlock {
            id: "b_0002".into(),
            kind: "app".into(),
            rev: 11,
            payload: json!({ "src": "/apps/x" }),
        },
    ];

    let doc = ReportDoc::from_blocks_exact("summary", &blocks).unwrap();
    assert_eq!(doc.doc_rev().unwrap(), 0);
    assert_eq!(doc.blocks_snapshot().unwrap(), blocks);
    assert_eq!(doc.project().unwrap().0, "summary");
}

#[test]
fn from_blocks_exact_accepts_consistent_empty_snapshot() {
    let doc = ReportDoc::from_blocks_exact("", &[]).unwrap();
    assert_eq!(doc.doc_rev().unwrap(), 0);
    assert_eq!(doc.blocks_snapshot().unwrap(), Vec::<ReportBlock>::new());
    assert_eq!(doc.block_index().unwrap(), Vec::new());
    assert_eq!(doc.project().unwrap(), (String::new(), String::new()));
}

#[test]
fn from_blocks_exact_rejects_duplicate_ids_before_layout_write() {
    let blocks = vec![
        ReportBlock {
            id: "b_dupe".into(),
            kind: "prose".into(),
            rev: 3,
            payload: json!({ "markdown": "first\n" }),
        },
        ReportBlock {
            id: "b_dupe".into(),
            kind: "prose".into(),
            rev: 9,
            payload: json!({ "markdown": "second\n" }),
        },
    ];

    let error = match ReportDoc::from_blocks_exact("summary", &blocks) {
        Ok(_) => panic!("duplicate ids must fail before the layout writer can remint them"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("duplicate block id b_dupe"));
}
