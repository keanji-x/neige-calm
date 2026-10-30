use super::raw_tests::Fixture;
use super::*;

async fn json_fixture(text: &str) -> Fixture {
    Fixture::new(&format!("CREATE TABLE worker_sessions(id,handle_state_json); INSERT INTO worker_sessions VALUES(1,'{}');", text.replace('\'', "''"))).await
}
#[tokio::test]
async fn u13_p1_json_duplicates_keep_both_ancestors_and_descendants() {
    let f = json_fixture(r#"{"a":{"x":1,"x":[2]},"\u0061":{"x":{"~/":null}}}"#).await;
    let a = f.audit().await;
    let facts = source_facts(&a).unwrap();
    let member = |name: &str, ordinal, occurrence| JsonStep::Member {
        name: name.into(),
        ordinal,
        occurrence,
    };
    let expected = vec![
        vec![],
        vec![member("a", 0, 0)],
        vec![member("a", 0, 0), member("x", 0, 0)],
        vec![member("a", 0, 0), member("x", 1, 1)],
        vec![member("a", 0, 0), member("x", 1, 1), JsonStep::Index(0)],
        vec![member("a", 1, 1)],
        vec![member("a", 1, 1), member("x", 0, 0)],
        vec![member("a", 1, 1), member("x", 0, 0), member("~/", 0, 0)],
    ];
    let nodes = facts
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, SourceKind::Json(_)))
        .collect::<Vec<_>>();
    let paths = nodes
        .iter()
        .map(|n| match &n.source.location {
            SourceLocation::Cell {
                json: Some(path), ..
            } => path.0.clone(),
            _ => panic!("JSON binding"),
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, expected);
    assert_eq!(JsonAddress(paths[7].clone()).pointer(), "/a/x/~0~1");
    assert_eq!(facts.duplicates().len(), 2);
    for (duplicate, (first, repeated)) in facts.duplicates().iter().zip([(2, 3), (1, 5)]) {
        assert_eq!(duplicate.first, nodes[first].source);
        assert_eq!(duplicate.repeated, nodes[repeated].source);
    }
    let field = facts.nodes().iter().find(|n| matches!(&n.source.location, SourceLocation::Cell { column_name, json: None, .. } if column_name == "handle_state_json")).unwrap();
    for node in nodes {
        assert_eq!(node.source.evidence_sha256, field.source.evidence_sha256);
        assert_eq!(node.source.source_sha256, *a.source_sha256());
        assert_eq!(node.source.source_len, a.source_len());
    }
}
#[tokio::test]
async fn u13_p1_json_malformed_neighbor_and_ordinary_text() {
    let f = Fixture::new(
        r#"CREATE TABLE worker_sessions(id,handle_state_json,provider_execution_json,ordinary_json);
        INSERT INTO worker_sessions VALUES(1,'{"a":','{"ok":[]}','{"looks":"json"}'),
            (2,'null',X'7B7D','bad'),(3,NULL,'true','[]'),(4,7,'{}','null');
        CREATE TABLE future(handle_state_json); INSERT INTO future VALUES('not JSON');"#,
    )
    .await;
    let a = f.audit().await;
    let facts = source_facts(&a).unwrap();
    assert_eq!(
        facts
            .nodes()
            .iter()
            .filter(|n| n.blocked == BlockedReason::MalformedJson)
            .count(),
        1
    );
    assert_eq!(
        facts
            .nodes()
            .iter()
            .filter(|n| n.blocked == BlockedReason::InvalidJsonStorage)
            .count(),
        2
    );
    assert!(facts.nodes().iter().any(|n| matches!(&n.source.location,
        SourceLocation::Cell { json: Some(p), .. } if p.pointer() == "/ok")));
    let ordinary = facts.nodes().iter().filter(|n| matches!(&n.source.location,
        SourceLocation::Cell { row, column_name, .. } if column_name == "ordinary_json" || row.relation == "future")).collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 5);
    assert!(
        ordinary
            .iter()
            .all(|n| n.blocked == BlockedReason::Unclassified && n.kind == SourceKind::Cell)
    );
    assert_eq!(
        facts
            .nodes()
            .iter()
            .filter(|n| n.kind == SourceKind::SqlNull)
            .count(),
        1
    );
    assert!(
        facts
            .nodes()
            .iter()
            .any(|n| n.kind == SourceKind::Json(JsonKind::Null))
    );
}
#[tokio::test]
async fn u13_p1_json_incomplete_never_complete() {
    for text in [
        format!("{}0{}", "[".repeat(70), "]".repeat(70)),
        "1e400".into(),
        "[1e400,0]".into(),
        "[0,-1e400]".into(),
        "[1e999999999999999999999999,0]".into(),
        r#"{"x":1e400,"x":"valid \uD800\uDC00"}"#.into(),
        r#"["valid \uD800\uDC00",1e400]"#.into(),
        format!("[{}0]", "0,".repeat(100_000)),
    ] {
        let f = json_fixture(&text).await;
        let a = f.audit().await;
        assert!(matches!(
            source_facts(&a),
            Err(FactCoverageError::JsonTraversal { .. })
        ));
    }
}
#[tokio::test]
async fn u13_p1_json_syntax_errors_preserve_cell_without_partial_tree() {
    for text in [
        "{",
        "[1,]",
        "{} false",
        "01",
        r#""\uD800""#,
        "NaN",
        "1e400x",
        "[1e400,]",
        "",
    ] {
        let f = json_fixture(text).await;
        let a = f.audit().await;
        let facts = source_facts(&a).unwrap();
        assert_eq!(
            facts
                .nodes()
                .iter()
                .filter(|n| n.blocked == BlockedReason::MalformedJson)
                .count(),
            1
        );
        assert!(
            !facts
                .nodes()
                .iter()
                .any(|n| matches!(n.kind, SourceKind::Json(_)))
        );
        assert_eq!(
            raw_tests::relation(&a, "worker_sessions").rows[0].cells[1],
            RawCell::Text(text.as_bytes().to_vec())
        );
    }
}

#[tokio::test]
async fn u13_p1_json_overflow_malformed_still_visits_healthy_neighbor() {
    let mut malformed = vec![r#"[1e400,"\uD800"]"#.to_owned()];
    for invalid in [
        r#""\uD800""#,
        r#""\uDC00""#,
        r#""\uD800\u0041""#,
        r#""\uD800\uD800""#,
        r#""\uDC00\uD800""#,
        r#""\u12xz""#,
        r#""\x41""#,
        r#""\q""#,
        "\"unclosed",
        "\"raw\ncontrol\"",
        r#"{"\uD800":0}"#,
        r#"{"\uDC00":0}"#,
        r#"{"\q":0}"#,
        r#"{"a" 0}"#,
        "01",
        "1e",
        "1.",
        "true false",
    ] {
        malformed.push(format!("[1e400,{invalid}]"));
        malformed.push(format!("[{invalid},1e400]"));
        malformed.push(format!(r#"{{"overflow":1e400,"bad":{invalid}}}"#));
        malformed.push(format!(r#"{{"bad":{invalid},"overflow":1e400}}"#));
    }
    malformed.extend(
        [
            "[1e400,]",
            "[1e400",
            "[1e400] false",
            "{\"a\":1e400,}",
            "{\"a\":1e400",
            "1e400x",
            "[1e400:0]",
            "[1e400,,0]",
        ]
        .map(str::to_owned),
    );
    for text in malformed {
        let f = Fixture::new(&format!(
            r#"CREATE TABLE worker_sessions(id,handle_state_json,provider_execution_json);
            INSERT INTO worker_sessions VALUES(1,'{}','{{"healthy":[]}}');"#,
            text.replace('\'', "''"),
        ))
        .await;
        let a = f.audit().await;
        let facts = source_facts(&a).unwrap_or_else(|e| panic!("{text:?}: {e:?}"));
        let bad = facts
            .nodes()
            .iter()
            .filter(|n| {
                matches!(&n.source.location,
            SourceLocation::Cell { column_name, .. } if column_name == "handle_state_json")
            })
            .collect::<Vec<_>>();
        assert_eq!(bad.len(), 1, "{text:?}");
        assert_eq!(bad[0].blocked, BlockedReason::MalformedJson, "{text:?}");
        assert_eq!(bad[0].kind, SourceKind::Cell);
        assert_eq!(
            raw_tests::relation(&a, "worker_sessions").rows[0].cells[1],
            RawCell::Text(text.as_bytes().to_vec())
        );
        let healthy = facts
            .nodes()
            .iter()
            .filter_map(|n| match &n.source.location {
                SourceLocation::Cell {
                    column_name,
                    json: Some(p),
                    ..
                } if column_name == "provider_execution_json" => {
                    Some((p.pointer(), n.kind.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            healthy,
            vec![
                ("".into(), SourceKind::Json(JsonKind::Object)),
                ("/healthy".into(), SourceKind::Json(JsonKind::Array))
            ],
            "{text:?}"
        );
    }
}
