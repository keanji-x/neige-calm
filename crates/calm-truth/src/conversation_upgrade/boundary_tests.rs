use super::raw_tests::{Fixture, relation};
use super::*;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row};
use std::fs;

fn source_state(f: &Fixture) -> (Vec<u8>, Vec<std::ffi::OsString>) {
    let bytes = fs::read(&f.path).unwrap();
    let mut entries = fs::read_dir(f.dir.path())
        .unwrap()
        .map(|r| r.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    (bytes, entries)
}
#[tokio::test]
async fn snapshot_never_changes_source_or_creates_sidecars() {
    for mode in ["DELETE", "WAL"] {
        let f = Fixture::new(&format!(
            "PRAGMA journal_mode={mode}; CREATE TABLE old(v); INSERT INTO old VALUES('raw');"
        ))
        .await;
        let before = source_state(&f);
        assert_eq!(before.1, vec![std::ffi::OsString::from("backup.db")]);
        let mut oracle = f.oracle().await;
        let ddl: Vec<Option<String>> =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema ORDER BY rowid")
                .fetch_all(&mut oracle)
                .await
                .unwrap();
        oracle.close().await.unwrap();
        let snapshot = OfflineSnapshot::open(f.input()).await.unwrap();
        // The production connection itself refuses writes; the public API never
        // exposes it. This assertion observes SQLite's stable readonly code.
        let mut snapshot = snapshot;
        let e = sqlx::query("CREATE TABLE forbidden(v)")
            .execute(snapshot.connection_for_test())
            .await
            .unwrap_err();
        assert_eq!(e.as_database_error().unwrap().code().as_deref(), Some("8"));
        let a = snapshot.inventory().await.unwrap();
        assert_eq!(
            a.source_sha256().as_slice(),
            Sha256::digest(&before.0).as_slice()
        );
        assert_eq!(a.source_len(), before.0.len() as u64);
        assert_eq!(source_state(&f), before);
        let mut oracle = f.oracle().await;
        let after: Vec<Option<String>> =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema ORDER BY rowid")
                .fetch_all(&mut oracle)
                .await
                .unwrap();
        assert_eq!(after, ddl);
        oracle.close().await.unwrap();
        assert_eq!(source_state(&f), before);
    }
}

#[tokio::test]
async fn input_requires_explicit_freeze_and_missing_does_not_create() {
    let f = Fixture::new("CREATE TABLE old(v);").await;
    let mut input = f.input();
    input.freeze_evidence.clear();
    assert!(matches!(
        OfflineSnapshot::open(input).await,
        Err(SnapshotError::UnsupportedInput(_))
    ));
    let mut input = f.input();
    input.operator.clear();
    assert!(matches!(
        OfflineSnapshot::open(input).await,
        Err(SnapshotError::UnsupportedInput(_))
    ));
    let mut input = f.input();
    input.path = "relative.db".into();
    assert!(matches!(
        OfflineSnapshot::open(input).await,
        Err(SnapshotError::UnsupportedInput(_))
    ));
    let before = source_state(&f);
    let mut input = f.input();
    input.path = f.dir.path().join("missing.db");
    assert!(matches!(
        OfflineSnapshot::open(input).await,
        Err(SnapshotError::Partial {
            reason: PartialReason::Io(_),
            ..
        })
    ));
    assert_eq!(source_state(&f), before);
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_database_path_is_typed_unsupported() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new("CREATE TABLE old(v);").await;
    let path = f.dir.path().join(std::ffi::OsString::from_vec(vec![0x80]));
    fs::rename(&f.path, &path).unwrap();
    let mut input = f.input();
    input.path = path.clone();
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        OfflineSnapshot::open(input).await,
        Err(SnapshotError::UnsupportedInput(_))
    ));
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(fs::read_dir(f.dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn unsupported_extent_and_unstable_keys_are_partial() {
    for sql in [
        "CREATE VIRTUAL TABLE module_data USING fts5(v); INSERT INTO module_data VALUES('raw');",
        "CREATE TABLE shadows(rowid,_rowid_,oid); INSERT INTO shadows VALUES(1,2,3);",
        "CREATE TABLE shadows(rowid,_rowid_,oid,k PRIMARY KEY); INSERT INTO shadows VALUES(1,2,3,NULL);",
    ] {
        let f = Fixture::new(sql).await;
        let before = source_state(&f);
        let result = OfflineSnapshot::open(f.input())
            .await
            .unwrap()
            .inventory()
            .await;
        assert!(
            matches!(
                result,
                Err(SnapshotError::Partial {
                    reason: PartialReason::UnsupportedCoverage(_),
                    ..
                })
            ),
            "{result:?}"
        );
        assert_eq!(source_state(&f), before);
    }
}

#[tokio::test]
async fn malformed_sqlite_and_changed_source_are_partial() {
    let f = Fixture::new("CREATE TABLE old(v); INSERT INTO old VALUES('raw');").await;
    let mut snapshot = OfflineSnapshot::open(f.input()).await.unwrap();
    // Ensure the read snapshot exists before violating the declared freeze.
    sqlx::query("SELECT * FROM old")
        .fetch_all(snapshot.connection_for_test())
        .await
        .unwrap();
    let mut bytes = fs::read(&f.path).unwrap();
    bytes.push(0);
    fs::write(&f.path, &bytes).unwrap();
    assert!(matches!(
        snapshot.inventory().await,
        Err(SnapshotError::Partial {
            reason: PartialReason::SourceChanged,
            ..
        })
    ));
    fs::write(&f.path, b"not SQLite").unwrap();
    let result = match OfflineSnapshot::open(f.input()).await {
        Ok(s) => s.inventory().await,
        Err(e) => Err(e),
    };
    assert!(matches!(
        result,
        Err(SnapshotError::Partial {
            reason: PartialReason::Sql(_),
            ..
        })
    ));
}

#[tokio::test]
async fn missing_and_bad_semantics_are_complete_blocked_originals() {
    for sql in [
        "CREATE TABLE pre_identity(v); INSERT INTO pre_identity VALUES('{broken');",
        "CREATE TABLE database_identity(singleton,id,minted_at_ms); CREATE TABLE _sqlx_migrations(version,success,checksum);",
        "CREATE TABLE database_identity(singleton,id,minted_at_ms); INSERT INTO database_identity VALUES(NULL,X'FF','wrong'); CREATE TABLE _sqlx_migrations(version,success,checksum); INSERT INTO _sqlx_migrations VALUES('future',0,NULL);",
    ] {
        let f = Fixture::new(sql).await;
        let before = source_state(&f);
        let a = f.audit().await;
        assert_eq!(a.semantic_status(), "BlockedAudit/Unclassified");
        let mut oracle = f.oracle().await;
        for name in ["database_identity", "_sqlx_migrations", "pre_identity"] {
            let present: i64 =
                sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name=?")
                    .bind(name)
                    .fetch_one(&mut oracle)
                    .await
                    .unwrap();
            assert_eq!(
                a.inventory().relations.iter().any(|r| r.name == name),
                present == 1
            );
            if present == 1 {
                let rows = sqlx::query(&format!("SELECT * FROM {name}"))
                    .fetch_all(&mut oracle)
                    .await
                    .unwrap();
                assert_eq!(relation(&a, name).rows.len(), rows.len());
                if name == "database_identity" && !rows.is_empty() {
                    assert!(a.unclassified_fields().any(|d| d.relation == name
                        && d.key == [RawCell::Integer(1)]
                        && d.field == "id"));
                    assert_eq!(
                        relation(&a, name).rows[0].cells[1],
                        RawCell::Blob(rows[0].get(1))
                    );
                }
            }
        }
        oracle.close().await.unwrap();
        assert_eq!(source_state(&f), before);
    }
}
