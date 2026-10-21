use crate::support::{at, dump, endpoint, key, observe_and_freeze, ok, plan_request, raw};
use research_archive::catalog::{Catalog, Endpoint, KeyKind, Method, QueryValue};
use research_archive::{OpenError, Store};

// Released v1 contract from the deployed annual patch baseline (060e08a).
// Keep every field independent of compiled() and Endpoint::new's defaults.
// Future catalog changes must explicitly assess this v1 upgrade policy, not
// regenerate this historical fixture. This is test input, not upgrade policy.
const V1_KEY: &[(&str, QueryValue)] = &[("symbol", QueryValue::Key)];
const V1_ENDPOINTS: &[Endpoint] = &[
    v1_endpoint(
        "consensus",
        "/v1/quote/financial-consensus-detail",
        V1_KEY,
        KeyKind::Security,
    ),
    v1_endpoint(
        "forecast_eps",
        "/v1/quote/forecast-eps",
        V1_KEY,
        KeyKind::Security,
    ),
    v1_endpoint(
        "rating_latest",
        "/v1/quote/institution-rating-latest",
        V1_KEY,
        KeyKind::Security,
    ),
    v1_endpoint(
        "ratings",
        "/v1/quote/institution-ratings",
        V1_KEY,
        KeyKind::Security,
    ),
    v1_endpoint(
        "rating_detail",
        "/v1/quote/institution-ratings/detail",
        V1_KEY,
        KeyKind::Security,
    ),
    v1_endpoint(
        "valuation",
        "/v1/quote/valuation",
        &[
            ("symbol", QueryValue::Key),
            ("indicator", QueryValue::Fixed("pe")),
            ("range", QueryValue::Fixed("1")),
        ],
        KeyKind::Security,
    ),
    v1_endpoint(
        "index_constituents",
        "/v1/quote/index-constituents",
        &[
            ("symbol", QueryValue::Key),
            ("offset", QueryValue::Fixed("0")),
            ("limit", QueryValue::Fixed("1000")),
        ],
        KeyKind::Index,
    ),
];

const fn v1_endpoint(
    id: &'static str,
    path: &'static str,
    query: &'static [(&'static str, QueryValue)],
    key_kind: KeyKind,
) -> Endpoint {
    Endpoint {
        method: Method::Get,
        headers: &[("Accept-Language", "en")],
        dc_restrict: None,
        path,
        query,
        key_kind,
        // EndpointId's constructor is private; use new only to obtain the id.
        ..Endpoint::new(id, path, query, key_kind)
    }
}

fn legacy() -> Catalog {
    Catalog::new(V1_ENDPOINTS.to_vec()).unwrap()
}

#[tokio::test]
async fn explicit_upgrade_only_appends_and_preserves_historical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let old = Store::open_with_catalog(&path, &legacy()).await.unwrap();
    let plan = old
        .plan_run(&plan_request(1, &["NVDA.US"], &[]))
        .await
        .unwrap();
    old.freeze_membership(plan.run_id, at(1, 10)).await.unwrap();
    old.record_observation(
        plan.run_id,
        &key("NVDA.US"),
        endpoint("consensus"),
        at(1, 20),
        &ok(b"{\"eps\":1.2345678901234567890}"),
    )
    .await
    .unwrap();
    let before_coverage = old.coverage(plan.run_id).await.unwrap();
    let before_read = old
        .read_at(&key("NVDA.US"), endpoint("consensus"), at(1, 30))
        .await
        .unwrap();
    old.close().await;
    let mut conn = raw(&path, false).await;
    let before = dump(&mut conn).await;
    drop(conn);
    downgrade_fixture_to_v1(&path).await;
    assert!(Store::open(&path).await.is_err());
    Store::upgrade_annual_catalog(&path).await.unwrap();
    let mut conn = raw(&path, false).await;
    let after = dump(&mut conn).await;
    let legacy_rows: Vec<_> = after
        .iter()
        .filter(|r| !r.starts_with("endpoint|'consensus_annual'|"))
        .cloned()
        .collect();
    assert_eq!(legacy_rows, before);
    assert_eq!(after.len(), before.len() + 1);
    drop(conn);
    Store::upgrade_annual_catalog(&path).await.unwrap();
    let mut conn = raw(&path, false).await;
    assert_eq!(dump(&mut conn).await, after);
    drop(conn);
    let upgraded = Store::upgrade_market_catalog(&path).await.unwrap();
    assert_eq!(
        before_coverage,
        upgraded.coverage(plan.run_id).await.unwrap()
    );
    assert!(
        !upgraded
            .ever_planned(&key("NVDA.US"), endpoint("consensus_annual"))
            .await
            .unwrap()
    );

    let after_read = upgraded
        .read_at(&key("NVDA.US"), endpoint("consensus"), at(1, 30))
        .await
        .unwrap();
    assert_eq!(before_read, after_read);
    assert!(
        upgraded
            .read_at(&key("NVDA.US"), endpoint("consensus_annual"), at(1, 30))
            .await
            .unwrap()
            .is_none()
    );

    let next = upgraded
        .plan_run(&plan_request(2, &["NVDA.US"], &[]))
        .await
        .unwrap();
    assert!(
        upgraded
            .coverage(next.run_id)
            .await
            .unwrap()
            .iter()
            .any(|p| p.endpoint == endpoint("consensus_annual"))
    );
}

#[tokio::test]
async fn upgrade_rejects_changed_legacy_entry_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let entries: Vec<_> = legacy()
        .endpoints()
        .iter()
        .map(|e| {
            if e.id.as_str() == "consensus" {
                Endpoint::new(
                    "consensus",
                    "/changed",
                    &[("symbol", QueryValue::Key)],
                    KeyKind::Security,
                )
            } else {
                *e
            }
        })
        .collect();
    Store::open_with_catalog(&path, &Catalog::new(entries).unwrap())
        .await
        .unwrap()
        .close()
        .await;
    downgrade_fixture_to_v1(&path).await;
    use sqlx::Connection;
    let mut conn = raw(&path, false).await;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(version, 1, "exercise the actual legacy-upgrade branch");
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Store::upgrade_annual_catalog(&path).await,
        Err(OpenError::Catalog(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn upgrade_does_not_create_a_missing_archive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.sqlite3");
    assert!(Store::upgrade_annual_catalog(&path).await.is_err());
    assert!(!path.exists());
}

async fn downgrade_fixture_to_v1(path: &std::path::Path) {
    use research_archive::store::schema::DDL;
    let mut conn = raw(path, false).await;
    sqlx::raw_sql("DROP TABLE run_pair; DROP TABLE run_plan_v3; DROP TABLE run_endpoint;")
        .execute(&mut conn)
        .await
        .unwrap();
    // Reconstruct the byte-frozen released schema, retaining fixture row values.
    // The fixture was seeded through Store; this is not a copy of upgrade logic.
    for table in ["endpoint", "run_key"] {
        sqlx::raw_sql(&format!(
            "CREATE TEMP TABLE saved AS SELECT * FROM {table}; DROP TABLE {table};"
        ))
        .execute(&mut conn)
        .await
        .unwrap();
        let begin = DDL.find(&format!("CREATE TABLE {table} (")).unwrap();
        let end = begin + DDL[begin..].find(';').unwrap() + 1;
        sqlx::raw_sql(&DDL[begin..end])
            .execute(&mut conn)
            .await
            .unwrap();
        sqlx::raw_sql(&format!(
            "INSERT INTO {table} SELECT * FROM saved; DROP TABLE saved;"
        ))
        .execute(&mut conn)
        .await
        .unwrap();
        for name in ["no_update", "no_delete", "no_duplicate"] {
            let begin = DDL.find(&format!("CREATE TRIGGER {table}_{name}")).unwrap();
            let end = begin + DDL[begin..].find("END;").unwrap() + 4;
            sqlx::raw_sql(&DDL[begin..end])
                .execute(&mut conn)
                .await
                .unwrap();
        }
    }
    sqlx::raw_sql(
        "PRAGMA user_version = 1; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;",
    )
    .execute(&mut conn)
    .await
    .unwrap();
}

#[tokio::test]
async fn legacy_v1_reader_refuses_without_migrating_and_bad_schema_stays_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    Store::open_with_catalog(&path, &legacy())
        .await
        .unwrap()
        .close()
        .await;
    downgrade_fixture_to_v1(&path).await;
    let before = std::fs::read(&path).unwrap();
    assert!(research_archive::ReadOnlyStore::open(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    use sqlx::Connection;
    let mut conn = raw(&path, false).await;
    sqlx::query("DROP TRIGGER blob_no_delete")
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Store::upgrade_annual_catalog(&path).await,
        Err(OpenError::Schema(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn extra_legacy_endpoint_is_not_silently_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let mut entries = legacy().endpoints().to_vec();
    entries.push(Endpoint::new(
        "foreign",
        "/foreign",
        &[("symbol", QueryValue::Key)],
        KeyKind::Security,
    ));
    Store::open_with_catalog(&path, &Catalog::new(entries).unwrap())
        .await
        .unwrap()
        .close()
        .await;
    downgrade_fixture_to_v1(&path).await;
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Store::upgrade_annual_catalog(&path).await,
        Err(OpenError::Catalog(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn cli_upgrade_obeys_capture_lock_and_needs_no_token() {
    let env = crate::cli_env::Env::new();
    std::fs::create_dir_all(&env.data_dir).unwrap();
    let path = Store::path(&env.data_dir);
    Store::open_with_catalog(&path, &legacy())
        .await
        .unwrap()
        .close()
        .await;
    downgrade_fixture_to_v1(&path).await;
    let before = std::fs::read(&path).unwrap();
    let lock = research_archive::lock::DataDirLock::try_acquire(&env.data_dir).unwrap();
    let result = env.run(&["upgrade-annual-catalog"]);
    assert_eq!(result.status.code(), Some(research_archive::exit::LOCKED));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(lock);
    let result = env.run(&["upgrade-annual-catalog"]);
    assert!(
        result.status.success(),
        "{}",
        crate::cli_env::text(&result.stderr)
    );
    assert!(!env.token_path().exists());
    assert!(Store::open(&path).await.is_err());
    let result = env.run(&["upgrade-annual-catalog"]);
    assert!(result.status.success());
}

#[tokio::test]
async fn upgraded_open_legacy_run_keeps_index_members_and_can_complete_without_annual() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let old = Store::open_with_catalog(&path, &legacy()).await.unwrap();
    let plan = old
        .plan_run(&plan_request(1, &["NVDA.US"], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(
        &old,
        &plan,
        at(1, 10),
        &[(".SPX.US", &["NVDA.US", "MSFT.US"])],
    )
    .await;
    let before = old.coverage(plan.run_id).await.unwrap();
    old.close().await;
    downgrade_fixture_to_v1(&path).await;
    Store::upgrade_annual_catalog(&path).await.unwrap();
    let upgraded = Store::upgrade_market_catalog(&path).await.unwrap();
    assert_eq!(upgraded.coverage(plan.run_id).await.unwrap(), before);
    let annual = upgraded
        .record_observation(
            plan.run_id,
            &key("MSFT.US"),
            endpoint("consensus_annual"),
            at(1, 20),
            &ok(b"{}"),
        )
        .await;
    assert!(matches!(
        annual,
        Err(research_archive::StoreError::NotPlanned { .. })
    ));
    for (i, pair) in before.iter().enumerate() {
        if !pair.has_ok {
            upgraded
                .record_observation(
                    plan.run_id,
                    &pair.key,
                    pair.endpoint,
                    at(1, 30 + i as i64),
                    &ok(b"{}"),
                )
                .await
                .unwrap();
        }
    }
    upgraded
        .end_run(
            plan.run_id,
            at(1, 100),
            research_archive::types::Finish::Complete,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        upgraded.coverage(plan.run_id).await.unwrap().len(),
        before.len()
    );
    let next = upgraded
        .plan_run(&plan_request(2, &["NVDA.US"], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(
        &upgraded,
        &next,
        at(2, 10),
        &[(".SPX.US", &["NVDA.US", "MSFT.US"])],
    )
    .await;
    let new = upgraded.coverage(next.run_id).await.unwrap();
    assert_eq!(
        new.iter()
            .filter(|p| p.endpoint == endpoint("consensus_annual"))
            .count(),
        2
    );
    upgraded.close().await;
    let reader = research_archive::ReadOnlyStore::open(&path).await.unwrap();
    assert_eq!(
        reader.coverage(plan.run_id).await.unwrap().len(),
        before.len()
    );
    assert_eq!(reader.coverage(next.run_id).await.unwrap(), new);
}
