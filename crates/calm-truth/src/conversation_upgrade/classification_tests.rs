use super::{raw_tests::Fixture, *};
use serde_json::{Value, json};
use sqlx::{Connection, Row};

pub(super) fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
pub(super) fn worker_row(id: &str, state: &str, frame: &str) -> String {
    format!(
        "INSERT INTO worker_sessions VALUES({},{},{},'card','track','native','agent','codex',NULL,'planner','resumable');",
        quoted(id),
        quoted(state),
        quoted(frame)
    )
}
pub(super) const TABLES: &str = "CREATE TABLE worker_sessions(id TEXT, state TEXT, handle_state_json TEXT, card_id TEXT, track_id TEXT, thread_id TEXT, agent_session_id TEXT, provider TEXT, provider_execution_json TEXT, contract TEXT, mode TEXT); CREATE TABLE cards(id TEXT,session_id TEXT); INSERT INTO cards VALUES('card','w');";
pub(super) async fn fixture(frame: &str) -> Fixture {
    Fixture::new(&format!("{TABLES}{}", worker_row("w", "running", frame))).await
}
pub(super) async fn frame_audit(v: &Value) -> CompleteBlockedAudit {
    fixture(&v.to_string()).await.audit().await
}
pub(super) fn at(f: &FieldFact, path: &str) -> bool {
    f.expected_path.as_deref() == Some(path)
        || (f.expected_path.is_none()
            && matches!(&f.source.location,
        SourceLocation::Cell { json: Some(address), .. } if address.pointer() == path))
}
pub(super) fn blocked(w: &WorkerFacts, path: &str, reason: FactReason) -> bool {
    w.fields
        .iter()
        .any(|f| at(f, path) && f.interpretation == Interpretation::Blocked(reason.clone()))
}
pub(super) fn original() -> Value {
    // Production owner constructs the runtime header. V1-only fields are exact
    // historical storage, not a copy of the classifier or runtime decoder.
    let mut v =
        serde_json::to_value(calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(7))
            .unwrap();
    v.as_object_mut().unwrap().remove("canonical_inputs");
    v["schema_version"] = json!(1);
    for name in [
        "pending_queue",
        "pending_envelope_ids",
        "pending_entry_meta",
        "pending_message_ids",
    ] {
        v[name] = json!([]);
    }
    v["planner_compaction"] = Value::Null;
    v["projection_client_id"] = Value::Null;
    v["issued_input_segments"] = Value::Null;
    v
}
#[tokio::test]
async fn u13_p1_facts_retired_orphan_sql_oracle() {
    let ids = [
        ("w", "running"),
        ("start", "starting"),
        ("idle", "idle"),
        ("pending", "turn_pending"),
        ("exit", "exited"),
        ("fail", "failed"),
        ("supersede", "superseded"),
        ("retired", "closed"),
        ("replaced", "replaced"),
        ("orphan", "unknown"),
    ];
    let body = original().to_string();
    let inserts: String = ids
        .iter()
        .map(|(id, state)| worker_row(id, state, &body))
        .collect();
    let fixture = Fixture::new(&format!(
        "{TABLES}{inserts} CREATE TABLE tracks(id TEXT,closed_at INTEGER); INSERT INTO tracks VALUES('track',1); CREATE TABLE assistant_conversation_archives(card_id TEXT,archived_at_ms INTEGER); INSERT INTO assistant_conversation_archives VALUES('card',1); DELETE FROM cards; INSERT INTO cards VALUES('card','w'),('no-current',NULL),('dangling','missing'); UPDATE worker_sessions SET card_id='missing-resource' WHERE id='orphan'; UPDATE worker_sessions SET card_id='no-current' WHERE id='idle'; UPDATE worker_sessions SET card_id='dangling' WHERE id='pending';"
    ))
    .await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let independently_selected = super::fact_oracle::expected(&fixture).await;
    for worker in facts.workers() {
        assert!(
            independently_selected
                .iter()
                .any(|n| n.source == worker.source)
        );
        let expected_cells: Vec<_> = independently_selected.iter().filter(|n| matches!(&n.source.location,
            SourceLocation::Cell { row, .. } if matches!(&worker.source.location, SourceLocation::Row(w) if w == row))).collect();
        for cell in &expected_cells {
            assert!(worker.fields.iter().any(|f| f.source == cell.source));
        }
        assert_eq!(expected_cells.len(), 11);
    }
    let mut oracle = fixture.oracle().await;
    let rows = sqlx::query("SELECT rowid,id,state,hex(CAST(handle_state_json AS BLOB)) FROM worker_sessions ORDER BY rowid").fetch_all(&mut oracle).await.unwrap();
    assert_eq!(facts.workers().len(), rows.len());
    for row in rows {
        let key = vec![RawCell::Integer(row.get(0))];
        let id: String = row.get(1);
        let worker = facts
            .workers()
            .iter()
            .find(|w| matches!(&w.source.location, SourceLocation::Row(r) if r.raw_key == key))
            .unwrap();
        assert_eq!(
            worker.logical_key,
            WorkerKey::Stored(RawCell::Text(id.into_bytes()))
        );
        let state: String = row.get(2);
        assert_eq!(
            worker.fields.iter().any(|f| matches!(&f.source.location,
            SourceLocation::Cell { column_name, json: None, .. } if column_name == "state")
                && f.interpretation == Interpretation::Stored(json!(state))),
            !matches!(state.as_str(), "closed" | "replaced" | "unknown")
        );
        assert!(matches!(worker.frame, Some(FrameFacts::LegacyV1)));
        let bytes = hex::decode(row.get::<String, _>(3)).unwrap();
        let cell = RawCell::Text(bytes);
        let expected = evidence_digest("cell", &cell).unwrap();
        assert!(worker.fields.iter().any(|f| matches!(&f.source.location,
            SourceLocation::Cell { column_name, cell_sha256, .. } if column_name == "handle_state_json" && cell_sha256 == &expected)));
        assert!(worker.fields.iter().any(|f| matches!(&f.source.location,
            SourceLocation::Cell { column_name, .. } if column_name == "state")));
    }
    oracle.close().await.unwrap();
    assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
}

#[tokio::test]
async fn u13_p1_facts_bad_neighbor_continues() {
    let fixture = Fixture::new(&format!(
        "{TABLES}{}{}{}{}",
        worker_row("bad", "closed", "{bad"),
        worker_row(
            "duplicate",
            "closed",
            r#"{"schema_version":1,"mode":"harness","mode":"harness"}"#
        ),
        worker_row("future", "closed", r#"{"schema_version":99}"#),
        worker_row("w", "running", &original().to_string())
    ))
    .await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    let expected = [
        ("bad", None),
        ("duplicate", Some(FactReason::DuplicateMember)),
        ("future", Some(FactReason::UnsupportedVersion)),
        ("w", None),
    ];
    assert_eq!(facts.workers().len(), expected.len());
    for (id, reason) in expected {
        let worker = facts
            .workers()
            .iter()
            .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(id.as_bytes().into())))
            .unwrap();
        let authority = Interpretation::Blocked(FactReason::MissingProviderAuthority);
        assert!(worker.fields.iter().any(|f| f.interpretation == authority));
        if id == "w" {
            assert!(matches!(worker.frame, Some(FrameFacts::LegacyV1)));
        }
        if let Some(reason) = reason {
            assert!(
                worker
                    .fields
                    .iter()
                    .any(|f| f.interpretation == Interpretation::Blocked(reason.clone()))
            );
        } else if id == "bad" {
            assert!(
                worker
                    .fields
                    .iter()
                    .any(|f| f.interpretation == Interpretation::Blocked(FactReason::Malformed))
            );
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_missing_headers_original_parent_and_unknown_children() {
    for name in super::legacy::HEADER {
        let mut v = original();
        v.as_object_mut().unwrap().remove(name);
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        assert!(
            blocked(w, &format!("/{name}"), FactReason::Missing),
            "{name}"
        );
        let missing = w
            .fields
            .iter()
            .find(|f| f.expected_path.as_deref() == Some(&format!("/{name}")))
            .unwrap();
        assert!(
            matches!(&missing.source.location, SourceLocation::Cell { json: Some(a), .. } if a.0.is_empty())
        );
    }
    let mut v = original();
    v["future"] = json!({"nested":[{"authority":true}]});
    v["issued_input_segments"] = json!({"turn_id":"old-turn","segments":[{"presentation":"system_report_edited","text":"old","attachments":[]}]});
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let w = &facts.workers()[0];
    assert!(blocked(
        w,
        "/future/nested/0/authority",
        FactReason::Unclassified
    ));
    assert!(
        w.fields
            .iter()
            .any(|f| at(f, "/issued_input_segments/turn_id")
                && f.interpretation == Interpretation::Stored(json!("old-turn")))
    );
    assert!(blocked(w, "/projection_client_id", FactReason::Null));
}

#[tokio::test]
async fn u13_p1_facts_key_storage_and_absent_store() {
    for sql in [
        "",
        TABLES,
        "CREATE TABLE worker_sessions(state TEXT); INSERT INTO worker_sessions VALUES('running');",
    ] {
        let audit = Fixture::new(sql).await.audit().await;
        let facts = classify_facts(&audit).unwrap();
        assert_eq!(
            matches!(facts.worker_store, TableEvidence::Absent { .. }),
            sql.is_empty()
        );
        if let Some(w) = facts.workers().first() {
            assert_eq!(
                w.logical_key,
                WorkerKey::Unidentified {
                    original: FieldValue::Missing,
                    reason: FactReason::Missing
                }
            );
            for reason in [
                FactReason::Missing,
                FactReason::MissingFrame,
                FactReason::MissingProviderAuthority,
            ] {
                assert!(w.fields.iter().any(|f| f.source == w.source
                    && f.expected_path.is_some()
                    && f.interpretation == Interpretation::Blocked(reason.clone())));
            }
        }
        assert_eq!(
            facts.workers().len(),
            usize::from(sql.contains("INSERT INTO worker_sessions"))
        );
    }
    for key in ["NULL", "X'80'", "''", "17"] {
        let fixture = Fixture::new(&format!(
            "{}{} UPDATE worker_sessions SET id={key};",
            TABLES.replace("id TEXT, state", "id, state"),
            worker_row("w", "running", &original().to_string())
        ))
        .await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        assert!(matches!(
            facts.workers()[0].logical_key,
            WorkerKey::Unidentified {
                original: FieldValue::Present(_),
                ..
            }
        ));
        let actual = facts.sources().nodes();
        for expected in super::fact_oracle::expected(&fixture).await {
            assert!(actual.iter().any(|n| n.source == expected.source));
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_frame_storage_early_returns_keep_neighbor() {
    for (sql, reason) in [
        ("NULL", FactReason::Null),
        ("X'80'", FactReason::Malformed),
        ("17", FactReason::Malformed),
        ("CAST(X'80' AS TEXT)", FactReason::Malformed),
        ("'{bad'", FactReason::Malformed),
        ("'false'", FactReason::Malformed),
        (
            "'{\"schema_version\":1,\"schema_version\":2}'",
            FactReason::DuplicateMember,
        ),
    ] {
        let f = Fixture::new(&format!(
            "{}{}{} UPDATE worker_sessions SET handle_state_json={sql} WHERE id='bad';",
            TABLES.replace("handle_state_json TEXT", "handle_state_json"),
            worker_row("bad", "failed", &original().to_string()),
            worker_row("w", "running", &original().to_string())
        ))
        .await;
        let audit = f.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&f, &facts).await;
        assert_eq!(facts.workers().len(), 2);
        let bad = facts
            .workers()
            .iter()
            .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"bad".to_vec())))
            .unwrap();
        assert!(bad.fields.iter().any(|f| matches!(&f.source.location,
            SourceLocation::Cell { column_name, .. } if column_name == "handle_state_json")
            && f.interpretation == Interpretation::Blocked(reason.clone())));
        if sql == "'false'" {
            let mut root = super::fact_oracle::expected(&f)
                .await
                .into_iter()
                .find(|n| matches!((&bad.source.location, &n.source.location),
                    (SourceLocation::Row(r), SourceLocation::Cell { row, column_name, json: None, .. })
                    if r == row && column_name == "handle_state_json"))
                .unwrap()
                .source;
            if let SourceLocation::Cell { json, .. } = &mut root.location {
                *json = Some(JsonAddress(vec![]));
            }
            assert!(bad.fields.iter().any(|f| f.source == root
                && f.expected_path.is_none()
                && f.interpretation == Interpretation::Blocked(FactReason::Malformed)));
            assert!(!bad.fields.iter().any(|f| matches!(&f.source.location,
                SourceLocation::Cell { column_name, .. } if column_name == "handle_state_json")
                && f.interpretation == Interpretation::Blocked(FactReason::Missing)));
            assert!(!bad.fields.iter().any(|f| matches!(&f.source.location,
                SourceLocation::Cell { column_name, json, .. } if column_name == "handle_state_json"
                    && (f.expected_path.as_deref() == Some("/schema_version")
                        || json.as_ref().is_some_and(|a| a.pointer() == "/schema_version")))));
        }
        let good = facts
            .workers()
            .iter()
            .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"w".to_vec())))
            .unwrap();
        assert!(matches!(good.frame, Some(FrameFacts::LegacyV1)));
        assert!(
            good.fields.iter().any(|f| at(f, "/schema_version")
                && f.interpretation == Interpretation::Stored(json!(1)))
        );
    }
    let f = Fixture::new("CREATE TABLE worker_sessions(id,state,provider_execution_json); INSERT INTO worker_sessions VALUES('bad','failed',NULL),('w','running',NULL);").await;
    let audit = f.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&f, &facts).await;
    assert_eq!(facts.workers().len(), 2);
    for w in facts.workers() {
        for reason in [FactReason::Missing, FactReason::MissingFrame] {
            assert!(w.fields.iter().any(|f| f.source == w.source
                && f.expected_path.as_deref() == Some("handle_state_json")
                && f.interpretation == Interpretation::Blocked(reason.clone())));
        }
    }
}
