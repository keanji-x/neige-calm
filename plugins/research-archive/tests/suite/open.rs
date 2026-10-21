//! `Store::open`: a new file gets the schema and catalog; an existing file
//! is refused on a `user_version`, schema or catalog mismatch.

use research_archive::catalog::{Catalog, Endpoint, KeyKind, QueryValue};
use research_archive::store::schema::DDL;
use research_archive::{OpenError, Store};

use crate::support::{dump, fresh, populated, raw};

async fn reopen(path: &std::path::Path) -> Result<Store, OpenError> {
    Store::open(path).await
}

#[tokio::test]
async fn new_file_gets_the_catalog_and_reopens_untouched() {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    let before = dump(&mut conn).await;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(version, 3);
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT endpoint_id FROM endpoint ORDER BY endpoint_id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(
        ids,
        [
            "consensus",
            "consensus_annual",
            "daily_candlesticks",
            "earnings_snapshot",
            "finance_calendar",
            "financial_reports",
            "forecast_eps",
            "index_constituents",
            "rating_detail",
            "rating_latest",
            "ratings",
            "valuation",
        ]
    );
    drop(conn);
    let store = reopen(&path).await.unwrap();
    store.close().await;
    let mut conn = raw(&path, false).await;
    assert_eq!(dump(&mut conn).await, before);
}

#[tokio::test]
async fn refuses_a_wrong_user_version() {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    match reopen(&path).await {
        Err(OpenError::UserVersion {
            found: 99,
            expected: 3,
        }) => {}
        other => panic!("{other:?}"),
    }
}

/// A refused file is left exactly as it was: same bytes, still in its own
/// journal mode, no WAL file.
#[tokio::test]
async fn refuses_a_foreign_database_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.sqlite3");
    std::fs::File::create(&path).unwrap();
    let mut conn = raw(&path, false).await;
    sqlx::raw_sql("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        reopen(&path).await,
        Err(OpenError::UserVersion {
            found: 0,
            expected: 3
        })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("other.sqlite3-wal").exists());
    let mut conn = raw(&path, false).await;
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(mode, "delete");
}

#[tokio::test]
async fn a_missing_file_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.sqlite3");
    Store::open(&path).await.unwrap().close().await;
    assert!(path.exists());
    reopen(&path).await.unwrap().close().await;
}

async fn schema_refusal(sql: &str) -> research_archive::store::SchemaDrift {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::raw_sql(sql).execute(&mut conn).await.unwrap();
    drop(conn);
    match reopen(&path).await {
        Err(OpenError::Schema(drift)) => drift,
        other => panic!("{sql}: {other:?}"),
    }
}

fn names(objects: &[(String, String)]) -> Vec<&str> {
    objects.iter().map(|(_, n)| n.as_str()).collect()
}

#[tokio::test]
async fn refuses_a_dropped_guard_trigger() {
    let drift = schema_refusal("DROP TRIGGER run_end_no_duplicate").await;
    assert_eq!(names(&drift.missing), ["run_end_no_duplicate"]);
    assert!(drift.extra.is_empty() && drift.altered.is_empty());
}

#[tokio::test]
async fn refuses_an_altered_guard_trigger() {
    // Same name and event, but the guard no longer aborts.
    let drift = schema_refusal(
        "DROP TRIGGER observation_no_update;
         CREATE TRIGGER observation_no_update BEFORE UPDATE ON observation
         WHEN 0 BEGIN SELECT RAISE(ABORT, 'observation is append-only'); END;",
    )
    .await;
    assert_eq!(names(&drift.altered), ["observation_no_update"]);
    assert!(drift.missing.is_empty() && drift.extra.is_empty());
}

#[tokio::test]
async fn refuses_an_extra_schema_object() {
    let drift =
        schema_refusal("CREATE TRIGGER sneaky AFTER INSERT ON blob BEGIN SELECT 1; END;").await;
    assert_eq!(names(&drift.extra), ["sneaky"]);
}

#[tokio::test]
async fn ddl_text_is_what_the_file_keeps() {
    // The comparison is by stored SQL text: one trigger statement of `DDL`
    // appears verbatim in a new file's schema.
    let f = fresh().await;
    f.store.close().await;
    let mut conn = raw(&f.path, false).await;
    let sql: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name = 'blob_dedup'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert!(DDL.contains(&format!("{sql};")), "{sql}");
}

async fn catalog_refusal(catalog: Vec<Endpoint>) -> research_archive::store::CatalogDrift {
    let (_dir, path) = populated().await;
    match Store::open_with_catalog(&path, &Catalog::new(catalog).unwrap()).await {
        Err(OpenError::Catalog(drift)) => drift,
        other => panic!("{other:?}"),
    }
}

fn compiled() -> Vec<Endpoint> {
    Catalog::compiled().endpoints().to_vec()
}

#[tokio::test]
async fn refuses_a_changed_catalog_row() {
    let changes: [fn(Endpoint) -> Endpoint; 4] = [
        |e: Endpoint| Endpoint { path: "/v2/x", ..e },
        |e: Endpoint| Endpoint {
            query: &[
                ("symbol", QueryValue::Key),
                ("range", QueryValue::Fixed("2")),
            ],
            ..e
        },
        |e: Endpoint| Endpoint {
            headers: &[("Accept-Language", "zh-CN")],
            ..e
        },
        |e: Endpoint| Endpoint {
            dc_restrict: Some(research_archive::catalog::DcRegion::Us),
            ..e
        },
    ];
    for change in changes {
        let mut catalog = compiled();
        catalog[5] = change(catalog[5]);
        let drift = catalog_refusal(catalog).await;
        assert_eq!(drift.changed, ["valuation"]);
        assert!(drift.missing.is_empty() && drift.extra.is_empty());
    }
}

/// A key kind change, kept valid (one index endpoint) by swapping two.
#[tokio::test]
async fn refuses_a_changed_key_kind() {
    let mut catalog = compiled();
    catalog[5].key_kind = KeyKind::Index;
    catalog[6].key_kind = KeyKind::Security;
    let drift = catalog_refusal(catalog).await;
    assert_eq!(drift.changed, ["valuation", "index_constituents"]);
}

#[tokio::test]
async fn refuses_an_added_catalog_row() {
    let mut catalog = compiled();
    catalog.push(Endpoint::new(
        "valuation_v2",
        "/v1/quote/valuation",
        &[("symbol", QueryValue::Key)],
        KeyKind::Security,
    ));
    let drift = catalog_refusal(catalog).await;
    assert_eq!(drift.missing, ["valuation_v2"]);
    assert!(drift.extra.is_empty() && drift.changed.is_empty());
}

#[tokio::test]
async fn refuses_a_removed_catalog_row() {
    let mut catalog = compiled();
    catalog.retain(|e| e.id.as_str() != "ratings");
    let drift = catalog_refusal(catalog).await;
    assert_eq!(drift.extra, ["ratings"]);
    assert!(drift.missing.is_empty() && drift.changed.is_empty());
}
