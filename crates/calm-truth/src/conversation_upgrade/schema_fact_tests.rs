use super::{raw_tests::Fixture, *};
use sha2::Digest;
use sqlx::{Connection, Row};

fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
fn metadata(stop: i64) -> String {
    let mut sql = "CREATE TABLE _sqlx_migrations(version INTEGER, description TEXT, installed_on TEXT, success INTEGER, checksum BLOB, execution_time INTEGER);".to_owned();
    for m in crate::MIGRATOR.iter().filter(|m| m.version <= stop) {
        sql.push_str(&format!(
            "INSERT INTO _sqlx_migrations VALUES({},{},'2026-01-01 00:00:00',1,X'{}',0);",
            m.version,
            quoted(&m.description),
            hex::encode(&m.checksum)
        ));
    }
    sql
}
fn checkpoint(stop: i64) -> String {
    let mut sql = metadata(stop);
    sql.push_str("CREATE TABLE tracks(id TEXT PRIMARY KEY); CREATE TABLE worker_sessions(id TEXT PRIMARY KEY,provider TEXT,thread_id TEXT); CREATE TABLE agent_backend_registry(provider TEXT,backend_id TEXT,PRIMARY KEY(provider,backend_id));");
    // Independent explicit fixture DDL, never extracted/parsed from migration SQL.
    if (101..127).contains(&stop) {
        sql.push_str("CREATE TABLE planner_recovery_threads(thread_id TEXT, track_id TEXT, card_id TEXT, registered_at_ms INTEGER); CREATE TABLE planner_recovery_issuances(id TEXT); CREATE TABLE planner_recovery_turns(id TEXT); CREATE TABLE planner_recovery_calls(id TEXT);");
    }
    if stop >= 110 {
        sql.push_str(
            "CREATE TABLE database_identity(singleton INTEGER,id TEXT,minted_at_ms INTEGER);",
        );
    }
    if stop >= 199 {
        sql.push_str("CREATE TABLE worker_session_backends(worker_session_id TEXT,provider TEXT,backend_id TEXT);");
    }
    if stop >= 203 {
        sql.push_str("CREATE TABLE claude_session_launches(worker_session_id TEXT,card_id TEXT,track_id TEXT,thread_id TEXT,execution_id TEXT,created_at_ms INTEGER);");
    }
    sql
}
fn field<'a>(fields: &'a [SchemaFieldFact], name: &str) -> &'a SchemaFieldFact {
    fields.iter().find(|f| f.expected_field == name).unwrap()
}
fn table<'a>(s: &'a SchemaFacts<'_>, name: &str) -> &'a HistoricalTableFact {
    s.historical_tables.iter().find(|f| f.name == name).unwrap()
}
fn version(m: &MigrationFact) -> Option<i64> {
    match field(&m.fields, "version").value {
        FieldValue::Present(RawCell::Integer(v)) => Some(v),
        _ => None,
    }
}
fn migration<'a>(s: &'a SchemaFacts<'_>, v: i64) -> &'a MigrationFact {
    s.migrations.iter().find(|m| version(m) == Some(v)).unwrap()
}
fn assert_cell(f: &SchemaFieldFact, relation: &str, key: i64, index: usize, cell: RawCell) {
    assert_eq!(f.value, FieldValue::Present(cell.clone()));
    let digest = evidence_digest("cell", &cell).unwrap();
    assert_eq!(f.source.evidence_sha256, digest);
    assert!(
        matches!(&f.source.location, SourceLocation::Cell { row, column_index, column_name, cell_sha256, json: None }
        if row.relation == relation && row.raw_key == [RawCell::Integer(key)] && *column_index == index && column_name == f.expected_field && *cell_sha256 == digest)
    );
}

async fn assert_raw_oracle(f: &SchemaFieldFact, fixture: &Fixture, relation: &str, key: i64) {
    let mut c = fixture.oracle().await;
    let column = f.expected_field;
    let row = sqlx::query(&format!("SELECT typeof(\"{column}\"),hex(CAST(\"{column}\" AS BLOB)),\"{column}\" FROM \"{relation}\" WHERE rowid=?"))
        .bind(key).fetch_one(&mut c).await.unwrap();
    let storage: String = row.get(0);
    let bytes = hex::decode(row.get::<String, _>(1)).unwrap();
    let cell = match storage.as_str() {
        "null" => RawCell::Null,
        "integer" => RawCell::Integer(row.get(2)),
        "real" => RawCell::RealBits(row.get::<f64, _>(2).to_bits()),
        "text" => RawCell::Text(bytes),
        "blob" => RawCell::Blob(bytes),
        _ => panic!("unexpected SQLite storage class"),
    };
    let index = match &f.source.location {
        SourceLocation::Cell { column_index, .. } => *column_index,
        _ => panic!("expected cell"),
    };
    assert_cell(f, relation, key, index, cell);
    assert_eq!(
        f.source.source_sha256.as_slice(),
        sha2::Sha256::digest(std::fs::read(&fixture.path).unwrap()).as_slice()
    );
    assert_eq!(
        f.source.source_len,
        std::fs::metadata(&fixture.path).unwrap().len()
    );
    c.close().await.unwrap();
}

async fn assert_metadata_oracle(fixture: &Fixture, s: &SchemaFacts<'_>) {
    let mut c = fixture.oracle().await;
    let rows =
        sqlx::query("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_schema ORDER BY rowid")
            .fetch_all(&mut c)
            .await
            .unwrap();
    let expected: Vec<_> = rows
        .iter()
        .map(|r| {
            vec![
                RawCell::Text(r.get::<String, _>(0).into_bytes()),
                RawCell::Text(r.get::<String, _>(1).into_bytes()),
                RawCell::Text(r.get::<String, _>(2).into_bytes()),
                RawCell::Integer(r.get(3)),
                r.get::<Option<String>, _>(4)
                    .map_or(RawCell::Null, |t| RawCell::Text(t.into_bytes())),
            ]
        })
        .collect();
    assert_eq!(s.inventory.schema, expected);
    assert_eq!(s.ddl.len(), expected.len());
    for (i, fact) in s.ddl.iter().enumerate() {
        assert_eq!(fact.source.location, SourceLocation::Schema { ordinal: i });
        assert_eq!((fact.field_index, fact.field_name), (4, "sql"));
        assert_eq!(fact.reason, SchemaReason::UninterpretedDdl);
    }
    for relation in &s.inventory.relations {
        let rows=sqlx::query("SELECT cid,name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo(?) ORDER BY cid").bind(&relation.name).fetch_all(&mut c).await.unwrap();
        let expected: Vec<_> = rows
            .iter()
            .map(|r| {
                vec![
                    RawCell::Integer(r.get(0)),
                    RawCell::Text(r.get::<String, _>(1).into_bytes()),
                    RawCell::Text(r.get::<String, _>(2).into_bytes()),
                    RawCell::Integer(r.get(3)),
                    r.get::<Option<String>, _>(4)
                        .map_or(RawCell::Null, |t| RawCell::Text(t.into_bytes())),
                    RawCell::Integer(r.get(5)),
                    RawCell::Integer(r.get(6)),
                ]
            })
            .collect();
        assert_eq!(relation.column_facts, expected);
    }
    c.close().await.unwrap();
}

// All three original S tests remain, with stronger per-cell assertions. The old
// DDL-substring "PresentAtSchema" positive is corrected to UnknownDdl: identical
// column names do not establish constraints/triggers or schema equivalence.
#[tokio::test]
async fn u13_p1_schema_checkpoints_sql_metadata_oracle() {
    for stop in [100, 101, 109, 110, 126, 127, 199, 203] {
        let fixture = Fixture::new(&checkpoint(stop)).await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.timeline, MigrationTimeline::KnownPrefix { through: stop });
        assert_metadata_oracle(&fixture, &s).await;
        let mut oracle = fixture.oracle().await;
        let rows = sqlx::query("SELECT rowid,version,description,hex(checksum),success,installed_on,execution_time FROM _sqlx_migrations ORDER BY version").fetch_all(&mut oracle).await.unwrap();
        assert_eq!(s.migrations.len(), rows.len());
        for row in rows {
            let v: i64 = row.get(1);
            let fact = migration(&s, v);
            assert_eq!(fact.fields.len(), 6);
            assert!(fact.fields.iter().all(|f| f.reasons.is_empty()));
            let key = row.get(0);
            assert!(
                matches!(&fact.source.location,SourceLocation::Row(r) if r.raw_key == [RawCell::Integer(key)])
            );
            let values = [
                RawCell::Integer(v),
                RawCell::Text(row.get::<String, _>(2).into_bytes()),
                RawCell::Text(row.get::<String, _>(5).into_bytes()),
                RawCell::Integer(row.get(4)),
                RawCell::Blob(hex::decode(row.get::<String, _>(3)).unwrap()),
                RawCell::Integer(row.get(6)),
            ];
            let raw_row = RawRow {
                key: vec![RawCell::Integer(key)],
                cells: values.to_vec(),
            };
            let digest = evidence_digest("row", &raw_row).unwrap();
            assert_eq!(fact.source.evidence_sha256, digest);
            assert!(matches!(&fact.source.location,SourceLocation::Row(r) if r.row_sha256==digest));
            for (i, value) in values.into_iter().enumerate() {
                assert_cell(&fact.fields[i], "_sqlx_migrations", key, i, value);
            }
        }
        for t in &s.historical_tables {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?",
            )
            .bind(t.name)
            .fetch_one(&mut oracle)
            .await
            .unwrap();
            let expected = if t.name.starts_with("planner_recovery_") && count == 1 {
                SchemaDisposition::LegacyRecoveryUnqualified
            } else if count == 1 {
                SchemaDisposition::UnknownDdl
            } else {
                SchemaDisposition::NotPresentAtRecordedPrefix
            };
            assert_eq!(t.disposition, expected, "{stop} {}", t.name);
            if count == 0 {
                assert!(
                    matches!(&t.source, TableEvidence::Absent { expected_table, schema, encoding } if *expected_table == t.name && schema.len() == audit.inventory().schema.len() && matches!(encoding.location,SourceLocation::Encoding))
                );
            }
        }
        assert_eq!(s.identity.status, IdentityStatus::Unidentified);
        oracle.close().await.unwrap();
    }
}
#[tokio::test]
async fn u13_p1_schema_bad_metadata_ddl_and_unknown_growth() {
    for bad in [
        "DELETE FROM _sqlx_migrations WHERE version=101;",
        "UPDATE _sqlx_migrations SET success=0 WHERE version=101;",
        "UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=101;",
        "UPDATE _sqlx_migrations SET description='future' WHERE version=101;",
        "INSERT INTO _sqlx_migrations SELECT * FROM _sqlx_migrations WHERE version=101;",
        "INSERT INTO _sqlx_migrations VALUES(999999,'future','date',1,X'',0);",
    ] {
        let fixture = Fixture::new(&format!("{}{bad}", checkpoint(127))).await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.timeline, MigrationTimeline::Unknown);
        if bad.starts_with("DELETE") {
            assert_eq!(
                s.missing_migrations
                    .iter()
                    .map(|m| m.expected_version)
                    .collect::<Vec<_>>(),
                [101]
            );
            assert!(
                matches!(&s.missing_migrations[0].parent,TableEvidence::Present(source) if matches!(&source.location,SourceLocation::Relation{name} if name=="_sqlx_migrations"))
            );
            assert_eq!(s.missing_migrations[0].reason, SchemaReason::Missing);
        }
        assert!(
            s.historical_tables
                .iter()
                .all(|t| t.disposition == SchemaDisposition::UnknownTimeline)
        );
    }
    let fixture = Fixture::new(&format!("{} DROP TABLE worker_session_backends; CREATE TABLE worker_session_backends(wrong TEXT); CREATE TABLE growing_unknown(new_column TEXT); INSERT INTO growing_unknown VALUES('kept');",checkpoint(203))).await;
    let audit = fixture.audit().await;
    let sources = source_facts(&audit).unwrap();
    let s = schema_facts(&sources).unwrap();
    assert_metadata_oracle(&fixture, &s).await;
    let t = table(&s, "worker_session_backends");
    assert_eq!(t.disposition, SchemaDisposition::Blocked);
    assert_eq!(
        t.missing_columns
            .iter()
            .map(|f| f.expected_field)
            .collect::<Vec<_>>(),
        ["worker_session_id", "provider", "backend_id"]
    );
    assert!(t.missing_columns.iter().all(
        |f| matches!(&f.source.location, SourceLocation::Relation { name } if name==t.name)
            && f.reasons == [SchemaReason::Missing]
    ));
    assert!(sources.nodes().iter().any(|n| matches!(&n.source.location,SourceLocation::Cell { column_name,row,.. } if row.relation=="growing_unknown" && column_name=="new_column") && n.blocked==BlockedReason::Unclassified));
    // Every actual schema tuple (including DDL) and xinfo tuple retains its raw digest.
    for node in sources.nodes() {
        let raw = match &node.source.location {
            SourceLocation::Schema { ordinal } => &s.inventory.schema[*ordinal],
            SourceLocation::ColumnMetadata { relation, ordinal } => {
                &s.inventory
                    .relations
                    .iter()
                    .find(|r| &r.name == relation)
                    .unwrap()
                    .column_facts[*ordinal]
            }
            _ => continue,
        };
        let domain = if matches!(node.source.location, SourceLocation::Schema { .. }) {
            "schema"
        } else {
            "xinfo"
        };
        assert_eq!(
            node.source.evidence_sha256,
            evidence_digest(domain, raw).unwrap()
        );
    }
}
#[tokio::test]
async fn u13_p1_schema_identity_never_mints_and_legacy_empty_or_nonempty() {
    for (rows, status) in [
        ("", IdentityStatus::Unidentified),
        (
            "INSERT INTO database_identity VALUES(1,'bad',0);",
            IdentityStatus::Unidentified,
        ),
        (
            "INSERT INTO database_identity VALUES(1,'11111111-1111-4111-8111-111111111111',0);",
            IdentityStatus::StoredIdentity("11111111-1111-4111-8111-111111111111".into()),
        ),
        (
            "INSERT INTO database_identity VALUES(1,'bad',0),(1,'other',0);",
            IdentityStatus::Unidentified,
        ),
    ] {
        let fixture = Fixture::new(&format!("{} CREATE TABLE database_identity(singleton INTEGER,id TEXT,minted_at_ms INTEGER);{rows}",metadata(109))).await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.identity.status, status);
        if rows.is_empty() {
            assert_eq!(s.identity.reasons, [SchemaReason::Missing]);
            assert_eq!(s.identity.missing_fields.len(), 3);
        }
        if rows.contains("other") {
            assert_eq!(s.identity.reasons, [SchemaReason::DuplicateIdentity]);
        }
        let mut oracle = fixture.oracle().await;
        let rows = sqlx::query(
            "SELECT rowid,singleton,id,minted_at_ms FROM database_identity ORDER BY rowid",
        )
        .fetch_all(&mut oracle)
        .await
        .unwrap();
        assert_eq!(s.identity.rows.len(), rows.len());
        for (i, row) in rows.iter().enumerate() {
            for (j, cell) in [
                RawCell::Integer(row.get(1)),
                RawCell::Text(row.get::<String, _>(2).into_bytes()),
                RawCell::Integer(row.get(3)),
            ]
            .into_iter()
            .enumerate()
            {
                assert_cell(
                    &s.identity.rows[i][j],
                    "database_identity",
                    row.get(0),
                    j,
                    cell,
                );
            }
        }
        assert_eq!(
            rows.len(),
            audit
                .inventory()
                .relations
                .iter()
                .find(|r| r.name == "database_identity")
                .unwrap()
                .rows
                .len()
        );
        oracle.close().await.unwrap();
    }
    for rows in [
        "",
        "INSERT INTO planner_recovery_threads VALUES('native','track','retired',1);",
    ] {
        let fixture = Fixture::new(&format!(
            "{} INSERT INTO tracks VALUES('track'); {rows}",
            checkpoint(109)
        ))
        .await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        for t in s
            .historical_tables
            .iter()
            .filter(|t| t.name.starts_with("planner_recovery_"))
        {
            assert_eq!(t.disposition, SchemaDisposition::LegacyRecoveryUnqualified);
            assert!(
                matches!(&t.source,TableEvidence::Present(source) if matches!(&source.location,SourceLocation::Relation {name} if name==t.name))
            );
        }
    }
}

#[tokio::test]
async fn u13_p1_schema_sqlx_unrecorded_execution_time() {
    let fixture = Fixture::new(&format!(
        "{} UPDATE _sqlx_migrations SET execution_time=-1 WHERE version=1;",
        checkpoint(110)
    ))
    .await;
    let audit = fixture.audit().await;
    let sources = source_facts(&audit).unwrap();
    let s = schema_facts(&sources).unwrap();
    let elapsed = field(&migration(&s, 1).fields, "execution_time");
    // Independent SQL oracle checks INTEGER, original -1 bytes and source hash.
    assert_raw_oracle(elapsed, &fixture, "_sqlx_migrations", 1).await;
    assert_cell(elapsed, "_sqlx_migrations", 1, 5, RawCell::Integer(-1));
    assert!(elapsed.reasons.is_empty(), "{:?}", elapsed.reasons);
    assert_eq!(s.timeline, MigrationTimeline::KnownPrefix { through: 110 });
    assert_eq!(
        table(&s, "planner_recovery_threads").disposition,
        SchemaDisposition::LegacyRecoveryUnqualified
    );
    assert_eq!(
        table(&s, "database_identity").disposition,
        SchemaDisposition::UnknownDdl
    );
}

#[tokio::test]
async fn u13_p1_schema_migration_field_errors_and_bad_neighbor() {
    use SchemaReason::*;
    for (name, value, expected) in [
        ("version", "NULL", Null),
        ("version", "'bad'", StorageType),
        ("version", "1.5", StorageType),
        ("version", "X'00'", StorageType),
        ("version", "-1", UnknownVersion),
        ("version", "999999", FutureVersion),
        ("description", "NULL", Null),
        ("description", "X'80'", StorageType),
        ("description", "CAST(X'80' AS TEXT)", InvalidEncoding),
        ("description", "'bad'", ReleasedMismatch),
        ("installed_on", "NULL", Null),
        ("installed_on", "X'00'", StorageType),
        ("installed_on", "CAST(X'80' AS TEXT)", InvalidEncoding),
        ("installed_on", "''", InvalidValue),
        ("installed_on", "'2026-02-30 00:00:00'", InvalidValue),
        ("success", "NULL", Null),
        ("success", "'bad'", StorageType),
        ("success", "0", FailedMigration),
        ("success", "2", InvalidValue),
        ("checksum", "NULL", Null),
        ("checksum", "'bad'", StorageType),
        ("checksum", "X'00'", ReleasedMismatch),
        ("execution_time", "NULL", Null),
        ("execution_time", "'bad'", StorageType),
        ("execution_time", "-2", InvalidValue),
    ] {
        let fixture = Fixture::new(&format!(
            "{} UPDATE _sqlx_migrations SET {name}={value} WHERE version=1;",
            metadata(2)
        ))
        .await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.timeline, MigrationTimeline::Unknown, "{name}={value}");
        let mut oracle = fixture.oracle().await;
        // Rowid is independent of the corrupted version and facts selection.
        let key: i64 = sqlx::query_scalar("SELECT rowid FROM _sqlx_migrations WHERE version IN (-1,999999) OR version IS NULL OR typeof(version)!='integer' OR version=1").fetch_one(&mut oracle).await.unwrap();
        let bad = s.migrations.iter().find(|m|matches!(&m.source.location,SourceLocation::Row(r) if r.raw_key==[RawCell::Integer(key)])).unwrap();
        assert_eq!(
            field(&bad.fields, name).reasons,
            [expected],
            "{name}={value}"
        );
        assert!(
            matches!(&field(&bad.fields,name).source.location, SourceLocation::Cell { column_name, row, .. } if column_name==name && row.raw_key==[RawCell::Integer(key)])
        );
        assert_raw_oracle(field(&bad.fields, name), &fixture, "_sqlx_migrations", key).await;
        assert!(migration(&s, 2).fields.iter().all(|f| f.reasons.is_empty()));
        oracle.close().await.unwrap();
    }
    let fixture = Fixture::new(&format!(
        "{} INSERT INTO _sqlx_migrations SELECT * FROM _sqlx_migrations WHERE version=1;",
        metadata(2)
    ))
    .await;
    let audit = fixture.audit().await;
    let sources = source_facts(&audit).unwrap();
    let s = schema_facts(&sources).unwrap();
    assert_eq!(
        s.migrations
            .iter()
            .filter(|m| field(&m.fields, "version").reasons == [DuplicateVersion])
            .count(),
        2
    );
    for name in [
        "version",
        "description",
        "installed_on",
        "success",
        "checksum",
        "execution_time",
    ] {
        let fixture = Fixture::new(&format!(
            "{} ALTER TABLE _sqlx_migrations DROP COLUMN {name};",
            metadata(2)
        ))
        .await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.timeline, MigrationTimeline::Unknown);
        assert_eq!(s.missing_migration_fields.len(), 1);
        assert_eq!(s.missing_migration_fields[0].expected_field, name);
        assert!(!s.migrations.is_empty());
        for m in &s.migrations {
            let f = field(&m.fields, name);
            assert_eq!(f.value, FieldValue::Missing);
            assert_eq!(f.reasons, [Missing]);
            assert_eq!(f.source, m.source);
        }
    }
}
#[tokio::test]
async fn u13_p1_schema_identity_field_matrix_and_absent_sources() {
    use SchemaReason::*;
    for (name, value, reason) in [
        ("singleton", "NULL", Null),
        ("singleton", "'bad'", StorageType),
        ("singleton", "1.5", StorageType),
        ("singleton", "0", InvalidValue),
        ("id", "NULL", Null),
        ("id", "X'00'", StorageType),
        ("id", "CAST(X'80' AS TEXT)", InvalidEncoding),
        ("id", "''", InvalidValue),
        ("id", "'bad'", InvalidValue),
        ("minted_at_ms", "NULL", Null),
        ("minted_at_ms", "'bad'", StorageType),
        ("minted_at_ms", "1.5", StorageType),
        ("minted_at_ms", "-1", InvalidValue),
    ] {
        let fixture = Fixture::new(&format!("CREATE TABLE database_identity(singleton,id,minted_at_ms); INSERT INTO database_identity VALUES(1,'11111111-1111-4111-8111-111111111111',0); UPDATE database_identity SET {name}={value};")).await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        assert_eq!(s.identity.status, IdentityStatus::Unidentified);
        let f = field(&s.identity.rows[0], name);
        assert_eq!(f.reasons, [reason]);
        assert!(
            matches!(&f.source.location,SourceLocation::Cell {column_name,..} if column_name==name)
        );
        assert_raw_oracle(f, &fixture, "database_identity", 1).await;
    }
    for name in ["singleton", "id", "minted_at_ms"] {
        let fixture=Fixture::new(&format!("CREATE TABLE database_identity(singleton,id,minted_at_ms); INSERT INTO database_identity VALUES(1,'11111111-1111-4111-8111-111111111111',0); ALTER TABLE database_identity DROP COLUMN {name};")).await;
        let audit = fixture.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        let f = field(&s.identity.rows[0], name);
        assert_eq!(f.value, FieldValue::Missing);
        assert_eq!(f.reasons, [Missing]);
        assert!(matches!(f.source.location, SourceLocation::Row(_)));
    }
    let fixture=Fixture::new("CREATE TABLE database_identity(singleton,id,minted_at_ms); INSERT INTO database_identity VALUES(1,'bad',-1),(NULL,NULL,NULL);").await;
    let audit = fixture.audit().await;
    let sources = source_facts(&audit).unwrap();
    let s = schema_facts(&sources).unwrap();
    assert_eq!(s.identity.reasons, [DuplicateIdentity]);
    assert!(
        s.identity
            .rows
            .iter()
            .flatten()
            .all(|f| f.reasons.contains(&DuplicateIdentity))
    );
    assert!(field(&s.identity.rows[1], "id").reasons.contains(&Null));
    for (i, row) in s.identity.rows.iter().enumerate() {
        for f in row {
            assert_raw_oracle(f, &fixture, "database_identity", i as i64 + 1).await;
        }
    }
    let fixture = Fixture::new("").await;
    let audit = fixture.audit().await;
    // F can own its unique ledger and S together, without a self reference.
    let (sources, s) = {
        let sources = source_facts(&audit).unwrap();
        let schema = schema_facts(&sources).unwrap();
        (sources, schema)
    };
    assert!(std::ptr::eq(sources.audit().inventory(), s.inventory));
    assert_eq!(s.identity.status, IdentityStatus::Unidentified);
    assert_eq!(s.identity.reasons, [Missing]);
    assert_eq!(
        s.identity
            .missing_fields
            .iter()
            .map(|f| f.expected_field)
            .collect::<Vec<_>>(),
        ["singleton", "id", "minted_at_ms"]
    );
    assert!(
        s.identity
            .missing_fields
            .iter()
            .all(|f| f.reason == Missing)
    );
    assert_eq!(s.missing_migration_fields.len(), 6);
    assert!(
        matches!(&s.identity.source,TableEvidence::Absent {expected_table:"database_identity",schema,..} if schema.is_empty())
    );
    assert!(matches!(
        s.migrations_source,
        TableEvidence::Absent {
            expected_table: "_sqlx_migrations",
            ..
        }
    ));
}
#[tokio::test]
async fn u13_p1_schema_timeline_contradictions_do_not_prove_ddl() {
    for (stop, tail, name) in [
        (
            109,
            "CREATE TABLE database_identity(singleton,id,minted_at_ms);",
            "database_identity",
        ),
        (110, "DROP TABLE database_identity;", "database_identity"),
        (
            199,
            "DROP TABLE worker_session_backends;",
            "worker_session_backends",
        ),
        (
            203,
            "DROP TABLE claude_session_launches;",
            "claude_session_launches",
        ),
    ] {
        let f = Fixture::new(&format!("{}{tail}", checkpoint(stop))).await;
        let audit = f.audit().await;
        let sources = source_facts(&audit).unwrap();
        let s = schema_facts(&sources).unwrap();
        let t = table(&s, name);
        assert_eq!(t.disposition, SchemaDisposition::Blocked);
        assert!(t.reasons.contains(&SchemaReason::TimelineContradiction));
    }
    let f = Fixture::new(&format!(
        "{} CREATE TABLE planner_recovery_threads(id);",
        checkpoint(127)
    ))
    .await;
    let audit = f.audit().await;
    let sources = source_facts(&audit).unwrap();
    let s = schema_facts(&sources).unwrap();
    let t = table(&s, "planner_recovery_threads");
    assert_eq!(t.disposition, SchemaDisposition::LegacyRecoveryUnqualified);
    assert!(t.reasons.contains(&SchemaReason::TimelineContradiction));
}
