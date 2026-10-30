use super::raw_tests::Fixture;
use super::*;

#[tokio::test]
async fn u13_p1_sources_no_omission_sql_oracle() {
    let f = Fixture::new(r#"
        CREATE TABLE worker_sessions(id TEXT PRIMARY KEY, state, card_id, handle_state_json TEXT, provider_execution_json TEXT);
        INSERT INTO worker_sessions VALUES
        ('retired','retired','missing','{}','null'), ('replaced','superseded','old','[]',NULL),
        ('closed','closed','closed','{}',NULL), ('archived','archived','archive','{}',NULL),
        ('orphan','future',NULL,'{}',NULL);
        CREATE TABLE cards(id, session_id); INSERT INTO cards VALUES('current','missing');
        CREATE TABLE tracks(id, closed_at); INSERT INTO tracks VALUES('closed',123);
        CREATE TABLE unknown_sentinel(id, future TEXT, extra BLOB);
        INSERT INTO unknown_sentinel VALUES(7,'not JSON',X'80'),(7,'{"x":1}',X'');
        CREATE TABLE empty_future(a DEFAULT 42);
        CREATE TABLE generated(a, stored AS (a+1) STORED, derived AS (a+2) VIRTUAL);
        INSERT INTO generated(a) VALUES(4);
        CREATE TABLE composite(a TEXT, b INTEGER, v, PRIMARY KEY(a,b)) WITHOUT ROWID;
        INSERT INTO composite VALUES('雪',2,X'00'),('z',1,1.5);
        CREATE VIEW empty_view AS SELECT a FROM empty_future;
        CREATE INDEX idx ON composite(v);
        CREATE TRIGGER inert AFTER UPDATE ON generated BEGIN SELECT 1; END;
        CREATE TABLE _sqlx_migrations(version, success, checksum);
        INSERT INTO _sqlx_migrations VALUES(999999,0,X'00FF');
        CREATE TABLE database_identity(singleton,id,minted_at_ms);
        INSERT INTO database_identity VALUES(1,NULL,-1);
    "#).await;
    let audit = f.audit().await;
    let facts = source_facts(&audit).unwrap();
    let actual = facts
        .nodes()
        .iter()
        .filter(|n| !matches!(n.kind, SourceKind::Json(_)))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(actual, fact_oracle::expected(&f).await);
    assert!(std::ptr::eq(facts.audit(), &audit));
}

#[tokio::test]
async fn u13_p1_sources_recursive_nodes_no_omission() {
    let f = Fixture::new(r#"CREATE TABLE worker_sessions(id,handle_state_json);
        INSERT INTO worker_sessions VALUES('old','{"future":{"container":{"child":[null,{},[],true,4,"x"]}}}');"#).await;
    let a = f.audit().await;
    let facts = source_facts(&a).unwrap();
    let actual = facts
        .nodes()
        .iter()
        .filter_map(|n| match &n.source.location {
            SourceLocation::Cell {
                json: Some(path), ..
            } => Some((path.pointer(), n.kind.clone(), path.0.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let member = |name: &str| JsonStep::Member {
        name: name.into(),
        ordinal: 0,
        occurrence: 0,
    };
    let base = vec![member("future"), member("container"), member("child")];
    let mut expected = vec![
        ("".into(), SourceKind::Json(JsonKind::Object), vec![]),
        (
            "/future".into(),
            SourceKind::Json(JsonKind::Object),
            base[..1].to_vec(),
        ),
        (
            "/future/container".into(),
            SourceKind::Json(JsonKind::Object),
            base[..2].to_vec(),
        ),
        (
            "/future/container/child".into(),
            SourceKind::Json(JsonKind::Array),
            base.clone(),
        ),
    ];
    for (i, kind) in [
        JsonKind::Null,
        JsonKind::Object,
        JsonKind::Array,
        JsonKind::Bool,
        JsonKind::Number,
        JsonKind::String,
    ]
    .into_iter()
    .enumerate()
    {
        let mut steps = base.clone();
        steps.push(JsonStep::Index(i));
        expected.push((
            format!("/future/container/child/{i}"),
            SourceKind::Json(kind),
            steps,
        ));
    }
    assert_eq!(actual, expected);
    assert!(
        facts
            .nodes()
            .iter()
            .all(|n| n.blocked == BlockedReason::Unclassified)
    );
}

#[tokio::test]
async fn u13_p1_sources_encoding_raw_bytes_and_derived() {
    for (encoding, invalid) in [("UTF-8", "80"), ("UTF-16le", "00D8"), ("UTF-16be", "D800")] {
        let f = Fixture::new(&format!(
            r#"PRAGMA encoding='{encoding}';
            CREATE TABLE worker_sessions(id,handle_state_json,ordinary_json TEXT);
            INSERT INTO worker_sessions VALUES(1,'{{"é雪":null}}','not JSON'),
                (2,CAST(X'{invalid}' AS TEXT),CAST(X'{invalid}' AS TEXT));
            CREATE TABLE generated(a, derived AS (a+1) VIRTUAL); INSERT INTO generated(a) VALUES(1);
        "#
        ))
        .await;
        let a = f.audit().await;
        let facts = source_facts(&a).unwrap();
        let mut actual = facts
            .nodes()
            .iter()
            .filter(|n| !matches!(n.kind, SourceKind::Json(_)))
            .cloned()
            .collect::<Vec<_>>();
        let bad = actual
            .iter_mut()
            .filter(|n| n.blocked == BlockedReason::InvalidJsonEncoding)
            .collect::<Vec<_>>();
        assert_eq!(bad.len(), 1);
        // Compare every original reference and digest, separately from diagnosis.
        for node in bad {
            node.blocked = BlockedReason::Unclassified;
        }
        assert_eq!(actual, fact_oracle::expected(&f).await);
        assert!(facts.nodes().iter().any(|n| matches!(&n.source.location,
            SourceLocation::Cell { json: Some(path), .. } if path.pointer() == "/é雪")));
    }
}
