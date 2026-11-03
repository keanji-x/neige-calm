use super::{classification_tests::*, *};
use serde_json::{Value, json};

// Exactly the original 25 explicit slots accepted by server HarnessSnapshot's
// public from_value_strict in fresh5; no serialization or default reconstruction.
fn complete() -> Value {
    json!([1,"harness","idle",["legacy"],42,
        [{"type":"user_message","text":"healthy"}], [9],
        [["entry",3,4,[]]], [["message"]], "thread","turn","report","head","issued",
        ["turn",[["user","healthy input",[],null]]],"projection",null,null,
        [1,2,null,3],null,null,false,null,null,["turn","head"]])
}
fn exact(w: &WorkerFacts, p: &str, i: Interpretation) -> bool {
    w.fields.iter().any(|f| {
        f.expected_path.is_none()
            && f.interpretation == i
            && matches!(&f.source.location, SourceLocation::Cell { column_name, json: Some(a), .. }
            if column_name == "handle_state_json" && a.pointer() == p)
    })
}
fn stored(w: &WorkerFacts, p: &str, v: Value) -> bool {
    exact(w, p, Interpretation::Stored(v))
}
fn deferred(facts: &CompleteFactAudit<'_>, w: &WorkerFacts, prefix: &str) -> usize {
    let mut count = 0;
    for n in facts.sources().nodes() {
        if matches!(&n.source.location, SourceLocation::Cell { row, column_name, json: Some(a), .. }
            if matches!(&w.source.location, SourceLocation::Row(r) if r == row)
                && column_name == "handle_state_json"
                && (a.pointer() == prefix || a.pointer().starts_with(&format!("{prefix}/"))))
        {
            count += 1;
            assert!(
                w.fields.iter().any(|f| f.source == n.source
                    && f.expected_path.is_none()
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::DeferredObservationInterpretation)),
                "missing Deferred {:?}",
                n.source.location
            );
            assert!(
                !w.fields.iter().any(|f| f.source == n.source
                    && matches!(f.interpretation, Interpretation::Stored(_)))
            );
        }
    }
    count
}

#[tokio::test]
async fn u13_p1_facts_whole_root_original_numeric_sources() {
    let v = complete();
    let fixture = super::raw_tests::Fixture::new(&format!(
        "{TABLES}{}{}{} DELETE FROM cards;",
        worker_row("retired", "closed", &v.to_string()),
        worker_row("orphan", "unknown", &v.to_string()),
        worker_row("w", "running", &original().to_string())
    ))
    .await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    assert_eq!(facts.workers().len(), 3);
    for w in &facts.workers()[..2] {
        assert!(matches!(w.frame, Some(FrameFacts::LegacyV1)));
        for p in [
            "/0",
            "/1",
            "/2",
            "/3/0",
            "/4",
            "/6/0",
            "/7/0/0",
            "/7/0/1",
            "/7/0/2",
            "/8/0/0",
            "/9",
            "/10",
            "/11",
            "/12",
            "/13",
            "/14/0",
            "/14/1/0/0",
            "/14/1/0/1",
            "/15",
            "/18/0",
            "/18/1",
            "/18/3",
            "/21",
            "/24/0",
            "/24/1",
        ] {
            assert!(stored(w, p, v.pointer(p).unwrap().clone()), "{p}");
        }
        for p in ["/5", "/6", "/7", "/8"] {
            assert!(exact(w, p, Interpretation::ArrayLength(1)), "{p}");
        }
        assert!(stored(w, "/14", v[14].clone()));
        assert!(stored(w, "/5/0/type", json!("user_message")));
        assert!(stored(w, "/5/0/text", json!("healthy")));
        for f in &w.fields {
            if let Some(p) = &f.expected_path {
                assert!(p.split('/').nth(1).unwrap().parse::<usize>().is_ok(), "{p}");
            }
        }
    }
    assert!(matches!(
        facts.workers()[2].frame,
        Some(FrameFacts::LegacyV1)
    ));
    assert!(stored(&facts.workers()[2], "/schema_version", json!(1)));
}

#[tokio::test]
async fn u13_p1_facts_whole_root_short_missing_never_default_sources() {
    for len in [3, 9] {
        let mut v = complete();
        v.as_array_mut().unwrap().truncate(len);
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        assert!(matches!(w.frame, Some(FrameFacts::LegacyV1)));
        assert!(stored(w, "/0", json!(1)));
        let p = if len == 3 { "/3" } else { "/9" };
        assert!(w.fields.iter().any(|f| f.expected_path.as_deref()==Some(p)
            && f.interpretation==Interpretation::Blocked(FactReason::Missing)
            && matches!(&f.source.location,SourceLocation::Cell {json:Some(a),..} if a.0.is_empty())));
        assert!(
            !facts
                .sources()
                .nodes()
                .iter()
                .any(|n| matches!(&n.source.location,
            SourceLocation::Cell {json:Some(a),..} if a.pointer()==p))
        );
        assert!(
            !w.fields.iter().any(|f| f.expected_path.is_some()
                && matches!(f.interpretation, Interpretation::Stored(_)))
        );
    }
}

#[tokio::test]
async fn u13_p1_facts_whole_root_parallel_holes_bad_siblings_and_identity() {
    let mut v = complete();
    v[3] = json!(["planner",false,false,7,["future",{"raw":[7,null]}]]);
    v[5] = json!([null,{"type":"user_message","text":"tail"},false]);
    v[6] = json!([false, 42]);
    v[7] = json!([["dup",false,false,[]],{"id":"dup","rev":3,"queued_at_ms":4,"attachments":[]}]);
    v[8] = json!([[false, "message"], ["message"]]);
    v[14] = json!([
        false,
        [[false, false, [], null], ["user", "healthy", [], null]]
    ]);
    v[18] = json!([false, 2, null, false]);
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    for p in [
        "/3/1",
        "/3/2",
        "/3/3",
        "/6/0",
        "/7/0/1",
        "/7/0/2",
        "/8/0/0",
        "/14/0",
        "/14/1/0/0",
        "/14/1/0/1",
        "/18/0",
        "/18/3",
    ] {
        assert!(
            exact(w, p, Interpretation::Blocked(FactReason::Malformed)),
            "{p}"
        );
    }
    for p in ["/7/0/0", "/7/1/id", "/8/0/1", "/8/1/0"] {
        assert!(
            exact(w, p, Interpretation::Blocked(FactReason::DuplicateIdentity)),
            "{p}"
        );
    }
    for p in ["/5", "/6", "/7", "/8"] {
        assert!(blocked(w, p, FactReason::UnequalLengths));
    }
    assert!(blocked(w, "/5/0", FactReason::Null));
    for p in ["/6/2", "/7/2", "/8/2"] {
        assert!(
            w.fields
                .iter()
                .any(|f| f.expected_path.as_deref() == Some(p)
                    && f.interpretation == Interpretation::Blocked(FactReason::Missing))
        );
    }
    for (p, val) in [
        ("/6/1", json!(42)),
        ("/7/1/rev", json!(3)),
        ("/14/1/1/1", json!("healthy")),
        ("/18/1", json!(2)),
    ] {
        assert!(stored(w, p, val), "{p}");
    }
    assert!(blocked(w, "/3/4/1", FactReason::AmbiguousRepresentation));
    assert!(stored(w, "/5/1/text", json!("tail")));
}

#[tokio::test]
async fn u13_p1_facts_whole_root_duplicate_numeric_observation_before_return() {
    // No Value parse of this source is used by the oracle or this assertion.
    let raw = r#"[1,"harness","idle",["legacy"],42,[{"type":"user_message","text":"first","text":{"raw":[null,7]}},null],[],[],[]]"#;
    let fixture = fixture(raw).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    assert!(w.frame.is_none());
    assert_eq!(facts.sources().duplicates().len(), 1);
    assert_eq!(deferred(&facts, w, "/5/0"), 7);
    assert_eq!(deferred(&facts, w, "/5/1"), 1);
    let dup = &facts.sources().duplicates()[0];
    assert_ne!(dup.first, dup.repeated);
    for src in [&dup.first, &dup.repeated] {
        assert!(w.fields.iter().any(|f| &f.source == src
            && f.expected_path.is_none()
            && f.interpretation
                == Interpretation::Blocked(FactReason::DeferredObservationInterpretation)));
    }
}

#[tokio::test]
async fn u13_p1_facts_whole_root_version_shape_and_provider_separation() {
    for (v, p, reason) in [
        (Value::Null, "", FactReason::Null),
        (json!(false), "", FactReason::Malformed),
        (json!("bad"), "", FactReason::Malformed),
        (json!([]), "/0", FactReason::Missing),
        (json!([null]), "/0", FactReason::Null),
        (json!([false]), "/0", FactReason::Malformed),
        (json!(["1"]), "/0", FactReason::Malformed),
        (json!([99]), "/0", FactReason::UnsupportedVersion),
        (
            json!({"schema_version":99}),
            "/schema_version",
            FactReason::UnsupportedVersion,
        ),
        (
            json!([2, "harness", {}]),
            "",
            FactReason::AmbiguousRepresentation,
        ),
    ] {
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        assert!(w.frame.is_none());
        assert!(blocked(w, p, reason), "{v}: {p}");
        if !v.is_object() {
            assert!(
                !w.fields
                    .iter()
                    .any(|f| f.expected_path.as_deref() == Some("/schema_version"))
            );
        }
    }
    for version in 2..=5 {
        for (checkpoint, prefix) in [
            (
                json!({"pending":[{"observation":{"raw":[null,7]}}]}),
                "/canonical_inputs/pending/0/observation",
            ),
            (
                json!([1,[{"observation":{"raw":[null,7]}}],null,[],[]]),
                "/canonical_inputs/1/0/observation",
            ),
        ] {
            let v = json!({"schema_version":version,"canonical_inputs":checkpoint});
            let fixture = fixture(&v.to_string()).await;
            let audit = fixture.audit().await;
            let facts = classify_facts(&audit).unwrap();
            let w = &facts.workers()[0];
            assert!(
                matches!(w.frame,Some(FrameFacts::DeferredReleased{original_version}) if original_version==version)
            );
            assert!(stored(w, "/schema_version", json!(version)));
            assert_eq!(deferred(&facts, w, prefix), 4);
        }
    }
    let mut v = complete();
    v.as_array_mut().unwrap().push(json!({"opaque":7}));
    let claim = json!([1,"execution",{"Turn":{"turn_id":"turn"}},"Held","Pending"]);
    let fixture = super::raw_tests::Fixture::new(&format!(
        "{TABLES}{} UPDATE worker_sessions SET provider_execution_json={};",
        worker_row("w", "running", &v.to_string()),
        quoted(&claim.to_string())
    ))
    .await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    assert!(blocked(w, "/25", FactReason::UnknownField));
    assert!(stored(w, "/24/1", json!("head")));
    assert!(w.fields.iter().any(|f| f.expected_path.is_none()
        && f.interpretation == Interpretation::Stored(json!("execution"))
        && matches!(&f.source.location,SourceLocation::Cell {column_name,json:Some(a),..}
            if column_name=="provider_execution_json" && a.pointer()=="/1")));
    assert!(!w.fields.iter().any(
        |f| matches!(&f.source.location,SourceLocation::Cell {column_name,..}
        if column_name=="provider_execution_json")
            && f.interpretation
                == Interpretation::Blocked(FactReason::DeferredObservationInterpretation)
    ));
}
