use super::*;

mod malformed_tests;

fn sample_payload() -> TrackReportPayload {
    TrackReportPayload::new(
        "planner agent did a thing",
        "# Goal\n\nReplace the foo with the bar.\n\n# Progress\n\nfoo->bar.\n",
    )
}

#[test]
fn doc_heads_is_stable_across_save_load_round_trips() {
    let mut doc = ReportDoc::from_payload(&sample_payload());
    let token = doc.doc_heads();
    assert!(token.starts_with("ah1:"), "scheme-tagged token: {token}");

    let mut reloaded = ReportDoc::from_bytes(&doc.to_bytes()).unwrap();
    assert_eq!(reloaded.doc_heads(), token);
    let mut reloaded_again = ReportDoc::from_bytes(&reloaded.to_bytes()).unwrap();
    assert_eq!(reloaded_again.doc_heads(), token);
}

#[test]
fn doc_heads_changes_on_any_edit() {
    let mut doc = ReportDoc::from_payload(&sample_payload());
    let before = doc.doc_heads();

    doc.update(
        "planner agent did a thing",
        "# Goal

changed.
",
    )
    .unwrap();
    let after_body = doc.doc_heads();
    assert_ne!(after_body, before, "body edit must move the heads");

    doc.update(
        "new summary",
        "# Goal

changed.
",
    )
    .unwrap();
    let after_summary = doc.doc_heads();
    assert_ne!(after_summary, after_body);

    assert_eq!(doc.doc_heads(), after_summary);
}

/// A pre-v2 doc: `summary` + `body` Texts at ROOT, no `blocks`/`order`.
fn legacy_doc_bytes(summary: &str, body: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    let summary_id = doc.put_object(&ROOT, FIELD_SUMMARY, ObjType::Text).unwrap();
    doc.update_text(&summary_id, summary).unwrap();
    let body_id = doc
        .put_object(&ROOT, LEGACY_FIELD_BODY, ObjType::Text)
        .unwrap();
    doc.update_text(&body_id, body).unwrap();
    doc.save()
}

#[test]
fn from_payload_then_project_returns_original_values() {
    let payload = sample_payload();
    let mut doc = ReportDoc::from_payload(&payload);
    let (summary, body) = doc.project().unwrap();
    assert_eq!(summary, payload.summary);
    assert_eq!(body, payload.body);
    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
    let (s2, b2) = reloaded.project().unwrap();
    assert_eq!(s2, payload.summary);
    assert_eq!(b2, payload.body);
    let index = reloaded.block_index().unwrap();
    assert_eq!(index.len(), 2);
    assert!(
        index
            .iter()
            .all(|(_, kind, rev)| kind == "prose" && *rev == 1)
    );
}

#[test]
fn from_payload_reuses_hint_block_ids() {
    let mut payload = sample_payload();
    let hint = reassign_ids(&[], &split_body(&payload.body));
    payload.blocks = Some(hint.clone());
    let doc = ReportDoc::from_payload(&payload);
    let index = doc.block_index().unwrap();
    assert_eq!(
        index
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>(),
        hint.iter()
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>(),
        "PR1-derived ids survive the CRDT seed"
    );
}

#[test]
fn from_payload_handles_empty_summary() {
    let payload = TrackReportPayload::new("", "# Goal\n");
    let mut doc = ReportDoc::from_payload(&payload);
    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
    let (s, b) = reloaded.project().unwrap();
    assert_eq!(s, "");
    assert_eq!(b, "# Goal\n");
}

#[test]
fn update_then_project_returns_new_values() {
    let payload = sample_payload();
    let mut doc = ReportDoc::from_payload(&payload);
    doc.update("new summary", "# Heading\n\nnew body.\n")
        .unwrap();
    let (s, b) = doc.project().unwrap();
    assert_eq!(s, "new summary");
    assert_eq!(b, "# Heading\n\nnew body.\n");
    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
    let (s2, b2) = reloaded.project().unwrap();
    assert_eq!(s2, "new summary");
    assert_eq!(b2, "# Heading\n\nnew body.\n");
}

#[test]
fn update_bumps_rev_only_for_changed_blocks() {
    let payload = TrackReportPayload::new("s", "# A\n\nalpha\n\n# B\n\nbeta\n");
    let mut doc = ReportDoc::from_payload(&payload);
    let before = doc.block_index().unwrap();
    assert_eq!(before.len(), 2);
    let (id_a, _, rev_a) = before[0].clone();
    let (id_b, _, rev_b) = before[1].clone();
    assert_eq!((rev_a, rev_b), (1, 1));

    doc.update("s", "# A\n\nalpha edited\n\n# B\n\nbeta\n")
        .unwrap();
    assert_eq!(
        doc.block_rev(&id_a).unwrap(),
        Some(2),
        "changed block: rev+1"
    );
    assert_eq!(
        doc.block_rev(&id_b).unwrap(),
        Some(1),
        "untouched block: rev unchanged"
    );

    doc.update("s", "# A\n\nalpha edited\n\n# B\n\nbeta\n")
        .unwrap();
    assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
    assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));

    doc.update("s", "# B\n\nbeta\n").unwrap();
    assert_eq!(
        doc.block_rev(&id_a).unwrap(),
        None,
        "vanished block is deleted"
    );
    assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));
    assert_eq!(doc.project().unwrap().1, "# B\n\nbeta\n");
}

#[test]
fn lazy_migration_preserves_projection_and_hint_ids() {
    let summary = "legacy summary";
    let body = "preamble\n\n# A\n\nalpha\n\n## B\n\nbeta\n";
    let bytes = legacy_doc_bytes(summary, body);

    let unmigrated = ReportDoc::from_bytes(&bytes).unwrap();
    assert_eq!(
        unmigrated.project().unwrap(),
        (summary.to_string(), body.to_string())
    );
    assert!(unmigrated.blocks_snapshot().unwrap().is_empty());

    let hint = reassign_ids(&[], &split_body(body));
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(
        doc.ensure_blocks_layout(Some(&hint)).unwrap(),
        "legacy doc migrates"
    );
    assert_eq!(
        doc.project().unwrap(),
        (summary.to_string(), body.to_string()),
        "projection is byte-identical"
    );
    assert_eq!(
        doc.block_index()
            .unwrap()
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>(),
        hint.iter()
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>(),
        "hint ids become the durable block ids"
    );
    assert!(doc.0.get(&ROOT, LEGACY_FIELD_BODY).unwrap().is_none());

    assert!(!doc.ensure_blocks_layout(Some(&hint)).unwrap());
    let bytes2 = doc.to_bytes();
    let mut reloaded = ReportDoc::from_bytes(&bytes2).unwrap();
    assert!(!reloaded.ensure_blocks_layout(None).unwrap());
    assert_eq!(
        reloaded.project().unwrap(),
        (summary.to_string(), body.to_string())
    );
}

#[test]
fn task_v4_migration_renames_terminal_goal_without_bumping_block_rev() {
    let legacy = ReportBlock {
        id: "b_terminal".into(),
        kind: calm_types::report_blocks::KIND_TASK.into(),
        rev: 7,
        payload: json!({
            "key": "compile",
            "kind": "terminal",
            "goal": "cargo check",
            "ready": true,
            "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR
        }),
    };
    let mut payload = TrackReportPayload::new("legacy", flat_text(&legacy));
    payload.schema_version = 3;
    payload.blocks = Some(vec![legacy]);
    let mut doc = ReportDoc::from_payload(&payload);

    assert!(doc.ensure_blocks_layout(payload.blocks.as_deref()).unwrap());
    let migrated = doc.blocks_snapshot().unwrap();
    assert_eq!(
        migrated[0].rev, 7,
        "a semantic rename preserves the CAS anchor"
    );
    assert_eq!(migrated[0].payload["command"], "cargo check");
    assert!(migrated[0].payload.get("goal").is_none());
    let body = doc.project().unwrap().1;
    assert!(body.contains(r#""command": "cargo check""#), "{body}");
    assert!(!body.contains(r#""goal""#), "{body}");
    assert!(
        !doc.ensure_blocks_layout(None).unwrap(),
        "migration is idempotent"
    );
}

#[test]
fn lazy_migration_without_hint_mints_ids() {
    let bytes = legacy_doc_bytes("s", "# A\n\nalpha\n");
    let mut doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert!(doc.ensure_blocks_layout(None).unwrap());
    let index = doc.block_index().unwrap();
    assert_eq!(index.len(), 1);
    assert!(
        index[0].0.starts_with("b_"),
        "minted b_xxxx id, got {}",
        index[0].0
    );
    assert_eq!(index[0].1, "prose");
    assert_eq!(index[0].2, 1);
}

#[test]
fn upsert_move_delete_block_round_trip() {
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", "# A\n\nalpha\n"));
    let (id_a, _, _) = doc.block_index().unwrap()[0].clone();

    let (id_b, rev_b) = doc.upsert_block(None, "prose", "# B\n\nbeta\n").unwrap();
    assert_eq!(rev_b, 1);
    assert!(id_b.starts_with("b_"));
    assert_ne!(id_b, id_a);
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha\n# B\n\nbeta\n");

    let (same_id, rev) = doc
        .upsert_block(Some(&id_a), "prose", "# A\n\nalpha v2\n")
        .unwrap();
    assert_eq!(same_id, id_a);
    assert_eq!(rev, 2);
    assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

    let (_, rev) = doc
        .upsert_block(Some(&id_a), "prose", "# A\n\nalpha v2\n")
        .unwrap();
    assert_eq!(rev, 2, "identical content: rev holds");
    assert_eq!(doc.block_rev(&id_a).unwrap(), Some(2));
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");

    assert!(doc.upsert_block(Some("b_nope"), "prose", "x").is_err());

    doc.move_block(&id_b, 0).unwrap();
    assert_eq!(doc.project().unwrap().1, "# B\n\nbeta\n# A\n\nalpha v2\n");
    assert_eq!(doc.block_rev(&id_b).unwrap(), Some(1));
    doc.move_block(&id_b, 1).unwrap();
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n# B\n\nbeta\n");
    assert!(doc.move_block(&id_b, 2).is_err());
    assert!(doc.move_block("b_nope", 0).is_err());

    doc.delete_block(&id_b).unwrap();
    assert_eq!(doc.project().unwrap().1, "# A\n\nalpha v2\n");
    assert_eq!(doc.block_rev(&id_b).unwrap(), None);
    assert!(doc.delete_block(&id_b).is_err(), "double delete errors");

    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).unwrap();
    assert_eq!(reloaded.project().unwrap().1, "# A\n\nalpha v2\n");
    assert_eq!(
        reloaded.block_index().unwrap(),
        vec![(id_a, "prose".to_string(), 2)]
    );
}

#[test]
fn large_repetitive_block_replacement_has_linear_time_bound() {
    let markdown = format!(
        "# Fixture\n\n{}\n\ncapture pending\n",
        "long-fixture-segment-".repeat(450)
    );
    assert!(markdown.len() > 9_000, "fixture must retain its scale");
    let mut doc = ReportDoc::from_payload(&TrackReportPayload::new("s", &markdown));
    let id = doc.block_index().unwrap()[0].0.clone();
    let blocks_id = doc.blocks_map().unwrap().unwrap();
    let entry_id = doc.entry_at(&blocks_id, &id).unwrap().unwrap();
    let text_id_before = doc
        .typed_at(&entry_id, KEY_TEXT, ObjType::Text)
        .unwrap()
        .unwrap();

    let started = std::time::Instant::now();
    doc.upsert_block(Some(&id), "prose", "[entity](neige://wave/source#b_target)")
        .unwrap();
    let elapsed = started.elapsed();
    let text_id_after = doc
        .typed_at(&entry_id, KEY_TEXT, ObjType::Text)
        .unwrap()
        .unwrap();

    assert_ne!(
        text_id_before, text_id_after,
        "changed block text must be replaced with a fresh Text object"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "9 KB replacement regressed beyond the smoke-test budget: {elapsed:?}"
    );
    assert_eq!(
        doc.project().unwrap().1,
        "[entity](neige://wave/source#b_target)"
    );
}

#[test]
fn identical_update_is_a_noop_at_byte_level() {
    let payload = sample_payload();
    let mut doc = ReportDoc::from_payload(&payload);
    let first = doc.to_bytes();
    doc.update(&payload.summary, &payload.body).unwrap();
    let second = doc.to_bytes();
    let r1 = ReportDoc::from_bytes(&first).unwrap();
    let r2 = ReportDoc::from_bytes(&second).unwrap();
    assert_eq!(
        r1.project().unwrap(),
        (payload.summary.clone(), payload.body.clone())
    );
    assert_eq!(r2.project().unwrap(), (payload.summary, payload.body));
    assert_eq!(
        r1.block_index().unwrap(),
        r2.block_index().unwrap(),
        "no-op update moves no revs"
    );
    assert!(
        second.len() <= first.len() * 2,
        "no-op update should not double the doc size: first={}, second={}",
        first.len(),
        second.len()
    );
}

#[test]
fn round_trip_preserves_multibyte_emoji_and_crlf() {
    let summary = "中文测试 🎉 🇨🇳";
    let body = "line1\r\nline2 中文 🎉 🇨🇳\r\n";
    let payload = TrackReportPayload::new(summary, body);

    let mut doc = ReportDoc::from_payload(&payload);
    let bytes = doc.to_bytes();
    let reloaded = ReportDoc::from_bytes(&bytes).expect("round-trip load");
    let (s, b) = reloaded.project().unwrap();
    assert_eq!(s.as_bytes(), summary.as_bytes());
    assert_eq!(b.as_bytes(), body.as_bytes());

    let mut doc2 = ReportDoc::from_bytes(&bytes).expect("re-load for update");
    let new_summary = "新摘要 🚀 🇯🇵";
    let new_body = "第一行\r\n第二行 🎊\r\n";
    doc2.update(new_summary, new_body).unwrap();
    let (s2, b2) = doc2.project().unwrap();
    assert_eq!(s2.as_bytes(), new_summary.as_bytes());
    assert_eq!(b2.as_bytes(), new_body.as_bytes());
    let bytes2 = doc2.to_bytes();
    let reloaded2 = ReportDoc::from_bytes(&bytes2).expect("post-update round-trip");
    let (s3, b3) = reloaded2.project().unwrap();
    assert_eq!(s3.as_bytes(), new_summary.as_bytes());
    assert_eq!(b3.as_bytes(), new_body.as_bytes());
}

#[test]
fn concurrent_fork_merge_preserves_both_edits() {
    let payload = TrackReportPayload::new("shared", "# A\n\nalpha\n\n# B\n\nbeta\n");
    let mut origin = ReportDoc::from_payload(&payload);
    let bytes = origin.to_bytes();

    let mut replica_a = ReportDoc::from_bytes(&bytes).unwrap();
    let mut replica_b = ReportDoc::from_bytes(&bytes).unwrap();

    replica_a
        .update("shared", "# A\n\nALPHA\n\n# B\n\nbeta\n")
        .unwrap();
    replica_b
        .update("shared", "# A\n\nalpha\n\n# B\n\nBETA\n")
        .unwrap();

    replica_a.0.merge(&mut replica_b.0).expect("merge replicas");
    let (merged_summary, merged_body) = replica_a.project().unwrap();
    assert_eq!(merged_summary, "shared", "summary stayed identical");
    assert!(
        merged_body.contains("ALPHA"),
        "replica A's edit survived: body = {merged_body:?}"
    );
    assert!(
        merged_body.contains("BETA"),
        "replica B's edit survived: body = {merged_body:?}"
    );
}

#[test]
fn same_block_concurrent_merge_loses_one_edit_without_a_production_merge_path() {
    // Tripwire: replacing a changed block's Text object loses one side of concurrent same-block edits;
    // safe only while production never merges two `body_crdt` docs.
    let payload = TrackReportPayload::new("shared", "# A\n\nalpha beta\n");
    let mut origin = ReportDoc::from_payload(&payload);
    let bytes = origin.to_bytes();

    let mut replica_a = ReportDoc::from_bytes(&bytes).unwrap();
    let mut replica_b = ReportDoc::from_bytes(&bytes).unwrap();
    replica_a.0.set_actor(automerge::ActorId::from([1_u8]));
    replica_b.0.set_actor(automerge::ActorId::from([2_u8]));

    replica_a.update("shared", "# A\n\nALPHA beta\n").unwrap();
    replica_b
        .update("shared", "# A\n\nalpha beta GAMMA\n")
        .unwrap();

    replica_a.0.merge(&mut replica_b.0).expect("merge replicas");
    assert_eq!(
        replica_a.project().unwrap().1,
        "# A\n\nalpha beta GAMMA\n",
        "object replacement currently resolves the conflict by losing replica A's edit"
    );
}
