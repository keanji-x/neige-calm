//! v3 persistence and public planning contracts, including a genuine v2 input.
use crate::support::{at, dump, endpoint, key, ok, plan_request, raw};
use research_archive::types::{Origin, RunId};
use research_archive::{Catalog, OpenError, Store};
use sqlx::Connection;

async fn released_v2(path: &std::path::Path) {
    std::fs::File::create(path).unwrap();
    let mut conn = raw(path, false).await;
    sqlx::raw_sql(research_archive::store::schema::DDL)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::raw_sql(research_archive::store::schema::MIGRATION_V2)
        .execute(&mut conn)
        .await
        .unwrap();
    for e in Catalog::released_v2().endpoints() {
        sqlx::query("INSERT INTO endpoint VALUES(?,?,?,?,?,?,?)")
            .bind(e.id.as_str())
            .bind(e.method.as_str())
            .bind(e.path)
            .bind(e.query_json())
            .bind(e.headers_json())
            .bind(Option::<String>::None)
            .bind(e.key_kind.as_str())
            .execute(&mut conn)
            .await
            .unwrap();
    }
    sqlx::raw_sql("INSERT INTO run VALUES(1,'longbridge','2026-01-02',86400000,144000000,86400001);
        INSERT INTO run_key VALUES(1,'NVDA.US','config');
        INSERT INTO run_key VALUES(1,'.SPX.US','index-symbol');
        INSERT INTO run_endpoint SELECT 1,endpoint_id FROM endpoint;
        INSERT INTO blob VALUES('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',X'7B2262223A312E323334353637383930313233343536373839307D');
        INSERT INTO observation VALUES(1,'NVDA.US','consensus',1,86400010,'ok','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',NULL,NULL);").execute(&mut conn).await.unwrap();
}

#[tokio::test]
async fn v3_upgrade_preserves_bytes_and_old_unfrozen_plans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    released_v2(&path).await;
    let mut conn = raw(&path, false).await;
    let before = dump(&mut conn).await;
    conn.close().await.unwrap();
    assert!(matches!(
        Store::open(&path).await,
        Err(OpenError::UserVersion {
            found: 2,
            expected: 3
        })
    ));
    let store = Store::upgrade_market_catalog(&path).await.unwrap();
    let mut conn = raw(&path, false).await;
    let after = dump(&mut conn).await;
    let old: Vec<_> = after
        .into_iter()
        .filter(|r| !r.starts_with("endpoint|") || before.contains(r))
        .collect();
    assert_eq!(old, before);
    let fk = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert!(fk.is_empty());
    conn.close().await.unwrap();
    let original = store.coverage(RunId(1)).await.unwrap();
    assert_eq!(original.len(), 8);
    assert!(
        !store
            .ever_planned(&key("NVDA.US"), endpoint("daily_candlesticks"))
            .await
            .unwrap()
    );
    store
        .record_observation(
            RunId(1),
            &key(".SPX.US"),
            endpoint("index_constituents"),
            at(1, 20),
            &ok(br#"{"total":1,"stocks":[{"symbol":"MSFT.US"}]}"#),
        )
        .await
        .unwrap();
    store.freeze_membership(RunId(1), at(1, 30)).await.unwrap();
    let coverage = store.coverage(RunId(1)).await.unwrap();
    assert_eq!(coverage.len(), 15);
    assert!(coverage.iter().all(|p| !matches!(
        p.endpoint.as_str(),
        "daily_candlesticks" | "earnings_snapshot" | "financial_reports" | "finance_calendar"
    )));
    let row = store
        .read_at(&key("NVDA.US"), endpoint("consensus"), at(1, 100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.body, br#"{"b":1.2345678901234567890}"#);
    store.close().await;
    Store::open(&path).await.unwrap().close().await;
}

#[tokio::test]
async fn v3_plan_freezes_requests_and_only_bars_for_benchmarks() {
    let f = crate::support::fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["MSFT.US", "QQQ.US"], &[]))
        .await
        .unwrap();
    let coverage = f.store.coverage(plan.run_id).await.unwrap();
    // QQQ is explicitly also research: shared bar pair counts once.
    assert_eq!(coverage.len(), 25);
    let soxx: Vec<_> = coverage
        .iter()
        .filter(|p| p.key.as_str() == "SOXX.US")
        .collect();
    assert_eq!(soxx.len(), 1);
    assert_eq!(soxx[0].endpoint.as_str(), "daily_candlesticks");
    let market: Vec<_> = coverage
        .iter()
        .filter(|p| p.key.as_str() == "market:US")
        .collect();
    assert_eq!(market.len(), 1);
    assert_eq!(market[0].endpoint.as_str(), "finance_calendar");
    let request = f
        .store
        .pair_request(plan.run_id, &key("MSFT.US"), endpoint("daily_candlesticks"))
        .await
        .unwrap();
    assert!(request.contains(&("start".into(), "2025-12-19".into())));
    assert!(request.contains(&("end".into(), "2026-01-02".into())));
    let resumed = f
        .store
        .plan_run(&plan_request(1, &["OTHER.US"], &[]))
        .await
        .unwrap();
    assert!(!resumed.created);
    assert_eq!(
        f.store
            .pair_request(plan.run_id, &key("MSFT.US"), endpoint("daily_candlesticks"))
            .await
            .unwrap(),
        request
    );
    assert!(
        !f.store
            .ever_planned(&key("OTHER.US"), endpoint("earnings_snapshot"))
            .await
            .unwrap()
    );
    let universe = f.store.universe_at(at(1, 20)).await.unwrap().unwrap();
    assert!(
        universe
            .members
            .contains(&(key("SOXX.US"), Origin::Benchmark))
    );
}

#[tokio::test]
async fn v3_request_tables_reject_every_replacement_surface() {
    let f = crate::support::fresh().await;
    f.store
        .plan_run(&plan_request(1, &["MSFT.US"], &[]))
        .await
        .unwrap();
    for recursive in [false, true] {
        let mut conn = raw(&f.path, recursive).await;
        for sql in [
            "UPDATE run_pair SET request_json='[]'",
            "DELETE FROM run_pair",
            "INSERT OR REPLACE INTO run_pair SELECT * FROM run_pair",
            "INSERT INTO run_pair SELECT * FROM run_pair WHERE true ON CONFLICT(run_id,key,endpoint_id) DO UPDATE SET request_json=excluded.request_json",
            "UPDATE run_plan_v3 SET run_id=run_id",
            "DELETE FROM run_plan_v3",
            "INSERT OR REPLACE INTO run_plan_v3 SELECT * FROM run_plan_v3",
        ] {
            assert!(sqlx::query(sql).execute(&mut conn).await.is_err(), "{sql}");
        }
    }
}

#[tokio::test]
async fn v3_upgrade_is_explicit_idempotent_and_rejects_drift_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    assert!(Store::upgrade_market_catalog(&path).await.is_err());
    assert!(!path.exists());
    released_v2(&path).await;
    let mut conn = raw(&path, false).await;
    sqlx::raw_sql("PRAGMA user_version=1; PRAGMA journal_mode=DELETE;")
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Store::upgrade_market_catalog(&path).await,
        Err(OpenError::UserVersion { found: 1, .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut conn = raw(&path, false).await;
    sqlx::query("PRAGMA user_version=2")
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    Store::upgrade_market_catalog(&path)
        .await
        .unwrap()
        .close()
        .await;
    let mut conn = raw(&path, false).await;
    let before = dump(&mut conn).await;
    conn.close().await.unwrap();
    Store::upgrade_market_catalog(&path)
        .await
        .unwrap()
        .close()
        .await;
    let mut conn = raw(&path, false).await;
    assert_eq!(dump(&mut conn).await, before);
    sqlx::raw_sql("DROP TRIGGER endpoint_no_update; UPDATE endpoint SET path='/drift' WHERE endpoint_id='consensus';").execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(Store::upgrade_market_catalog(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn configured_benchmarks_are_frozen_and_can_change_next_day() {
    let f = crate::support::fresh().await;
    let mut req = plan_request(1, &["MSFT.US"], &[]);
    req.benchmarks = vec![key("IWM.US"), key("MSFT.US")];
    let first = f.store.plan_run(&req).await.unwrap();
    assert_eq!(first.benchmarks, vec![key("IWM.US"), key("MSFT.US")]);
    let pairs = f.store.coverage(first.run_id).await.unwrap();
    let iwm: Vec<_> = pairs.iter().filter(|p| p.key == key("IWM.US")).collect();
    assert_eq!(iwm.len(), 1);
    assert_eq!(iwm[0].endpoint, endpoint("daily_candlesticks"));
    assert!(!pairs.iter().any(|p| p.key == key("QQQ.US")));
    assert_eq!(
        pairs
            .iter()
            .filter(|p| p.key == key("MSFT.US") && p.endpoint == endpoint("daily_candlesticks"))
            .count(),
        1
    );
    req.benchmarks = vec![key("DIA.US")];
    let resumed = f.store.plan_run(&req).await.unwrap();
    assert!(!resumed.created);
    assert_eq!(resumed.benchmarks, first.benchmarks);
    assert_eq!(f.store.coverage(first.run_id).await.unwrap(), pairs);
    let mut tomorrow = plan_request(2, &["MSFT.US"], &[]);
    tomorrow.benchmarks = vec![];
    let next = f.store.plan_run(&tomorrow).await.unwrap();
    assert!(next.benchmarks.is_empty());
    assert!(
        !f.store
            .coverage(next.run_id)
            .await
            .unwrap()
            .iter()
            .any(|p| p.key == key("IWM.US"))
    );
}

#[tokio::test]
async fn core_checkpoint_requires_frozen_rows_is_immutable_and_keeps_aux_writable() {
    use research_archive::catalog::CaptureTier;
    let f = crate::support::fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["MSFT.US"], &[".SPX.US"]))
        .await
        .unwrap();
    assert!(f.store.complete_core(plan.run_id, at(1, 5)).await.is_err());
    f.store
        .freeze_membership(plan.run_id, at(1, 6))
        .await
        .unwrap();
    assert!(f.store.complete_core(plan.run_id, at(1, 7)).await.is_err());
    let pairs = f.store.coverage(plan.run_id).await.unwrap();
    for p in pairs.iter().filter(|p| p.tier == CaptureTier::Core) {
        let body = if p.endpoint == endpoint("index_constituents") {
            crate::support::constituents(&["MSFT.US"])
        } else {
            b"{}".to_vec()
        };
        f.store
            .record_observation(plan.run_id, &p.key, p.endpoint, at(1, 8), &ok(&body))
            .await
            .unwrap();
    }
    f.store.complete_core(plan.run_id, at(1, 9)).await.unwrap();
    f.store.complete_core(plan.run_id, at(1, 10)).await.unwrap();
    assert_eq!(
        f.store.core_complete_at(plan.run_id).await.unwrap(),
        Some(at(1, 9))
    );
    assert!(
        f.store
            .record_observation(
                plan.run_id,
                &key("MSFT.US"),
                endpoint("earnings_snapshot"),
                at(1, 8),
                &ok(b"{}")
            )
            .await
            .is_err()
    );
    f.store
        .record_observation(
            plan.run_id,
            &key("MSFT.US"),
            endpoint("earnings_snapshot"),
            at(1, 10),
            &ok(b"{}"),
        )
        .await
        .unwrap();
    let mut conn = raw(&f.path, false).await;
    for sql in [
        "UPDATE run_core_complete SET finished_at=0",
        "DELETE FROM run_core_complete",
        "INSERT INTO run_core_complete SELECT * FROM run_core_complete",
    ] {
        assert!(sqlx::query(sql).execute(&mut conn).await.is_err());
    }
}

#[tokio::test]
async fn capture_tiers_are_frozen_with_requests_not_reinterpreted_on_reopen() {
    use research_archive::catalog::CaptureTier;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let store = Store::open(&path).await.unwrap();
    let plan = store
        .plan_run(&plan_request(1, &["MSFT.US"], &[]))
        .await
        .unwrap();
    let original = store.coverage(plan.run_id).await.unwrap();
    store.close().await;
    let mut endpoints = Catalog::compiled().endpoints().to_vec();
    endpoints
        .iter_mut()
        .find(|e| e.id.as_str() == "earnings_snapshot")
        .unwrap()
        .tier = CaptureTier::Core;
    let catalog = Catalog::new(endpoints).unwrap();
    let store = Store::open_with_catalog(&path, &catalog).await.unwrap();
    assert_eq!(store.coverage(plan.run_id).await.unwrap(), original);
    let next = store
        .plan_run(&plan_request(2, &["MSFT.US"], &[]))
        .await
        .unwrap();
    assert_eq!(
        store
            .coverage(next.run_id)
            .await
            .unwrap()
            .iter()
            .find(|p| p.endpoint.as_str() == "earnings_snapshot")
            .unwrap()
            .tier,
        CaptureTier::Core
    );
}
