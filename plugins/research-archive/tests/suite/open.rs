//! `Store::open`: a new file gets the schema and catalog; an existing file
//! is refused on a `user_version`, schema or catalog mismatch.

use research_archive::catalog::{Catalog, Endpoint, KeyKind, QueryValue};
use research_archive::store::schema::DDL;
use research_archive::{OpenError, Store};

use crate::support::{dump, fresh, populated, raw};

async fn reopen(path: &std::path::Path, catalog: &Catalog) -> Result<Store, OpenError> {
    Store::open(path, catalog).await
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
    assert_eq!(version, 1);
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let ids: Vec<String> = sqlx::query_scalar("SELECT endpoint_id FROM endpoint ORDER BY rowid")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        ids,
        [
            "consensus",
            "forecast_eps",
            "rating_latest",
            "ratings",
            "rating_detail",
            "valuation",
            "index_constituents"
        ]
    );
    drop(conn);
    let store = reopen(&path, &Catalog::compiled()).await.unwrap();
    store.close().await;
    let mut conn = raw(&path, false).await;
    assert_eq!(dump(&mut conn).await, before);
}

#[tokio::test]
async fn refuses_a_wrong_user_version() {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::query("PRAGMA user_version = 2")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    match reopen(&path, &Catalog::compiled()).await {
        Err(OpenError::UserVersion {
            found: 2,
            expected: 1,
        }) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn refuses_a_foreign_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.sqlite3");
    std::fs::File::create(&path).unwrap();
    let mut conn = raw(&path, false).await;
    sqlx::query("CREATE TABLE t (x)")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    assert!(matches!(
        reopen(&path, &Catalog::compiled()).await,
        Err(OpenError::UserVersion {
            found: 0,
            expected: 1
        })
    ));
}

async fn schema_refusal(sql: &str) -> research_archive::store::SchemaDrift {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::raw_sql(sql).execute(&mut conn).await.unwrap();
    drop(conn);
    match reopen(&path, &Catalog::compiled()).await {
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
    match reopen(&path, &Catalog::new(catalog).unwrap()).await {
        Err(OpenError::Catalog(drift)) => drift,
        other => panic!("{other:?}"),
    }
}

fn compiled() -> Vec<Endpoint> {
    Catalog::compiled().endpoints().to_vec()
}

#[tokio::test]
async fn refuses_a_changed_catalog_row() {
    let changes: [fn(Endpoint) -> Endpoint; 5] = [
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
        |e: Endpoint| Endpoint {
            key_kind: KeyKind::Index,
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
