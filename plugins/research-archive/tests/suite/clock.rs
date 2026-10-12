//! A run's clock is monotonic: no `fetched_at`, `frozen_at` or
//! `finished_at` before the run's high-water mark (equal is allowed). Also
//! the blob + observation transaction and the error class per endpoint.

use research_archive::StoreError;
use research_archive::types::{ErrorClass, ErrorDetail, FailureClass, Finish, Outcome, UnixMs};

use crate::support::{at, constituents, endpoint, failed, fresh, key, ok, plan_request, raw};

fn clock_went_back(r: Result<impl std::fmt::Debug, StoreError>, at: UnixMs, mark: UnixMs) {
    match r {
        Err(StoreError::ClockWentBack { at: a, mark: m, .. }) if a == at && m == mark => {}
        other => panic!("expected ClockWentBack {at:?} < {mark:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_observation_before_the_plan_is_refused() {
    let f = fresh().await;
    let mut req = plan_request(1, &["AAPL.US"], &[]);
    req.started_at = at(1, 10);
    let p = f.store.plan_run(&req).await.unwrap();
    let rec = |t| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(p.run_id, &key("AAPL.US"), endpoint("ratings"), t, &ok(b"r"))
                .await
        }
    };
    // Inside the window, but before `started_at`.
    clock_went_back(rec(at(1, 5)).await, at(1, 5), at(1, 10));
    assert_eq!(rec(at(1, 10)).await.unwrap(), 1, "equal to the mark");
}

/// The mark is the run's, not the pair's: a later write to any pair of the
/// run raises it. Other runs have their own clocks.
#[tokio::test]
async fn fetched_at_below_a_prior_fetched_at_is_refused() {
    let f = fresh().await;
    let d1 = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let rec = |run, e: &'static str, t| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(
                    run,
                    &key("AAPL.US"),
                    endpoint(e),
                    t,
                    &failed(FailureClass::Transient),
                )
                .await
        }
    };
    assert_eq!(rec(d1.run_id, "consensus", at(1, 20)).await.unwrap(), 1);
    clock_went_back(
        rec(d1.run_id, "ratings", at(1, 19)).await,
        at(1, 19),
        at(1, 20),
    );
    clock_went_back(
        rec(d1.run_id, "consensus", at(1, 19)).await,
        at(1, 19),
        at(1, 20),
    );
    assert_eq!(rec(d1.run_id, "consensus", at(1, 20)).await.unwrap(), 2);
    assert_eq!(rec(d1.run_id, "ratings", at(1, 20)).await.unwrap(), 1);
    let d2 = f
        .store
        .plan_run(&plan_request(2, &["AAPL.US"], &[]))
        .await
        .unwrap();
    assert_eq!(rec(d2.run_id, "consensus", at(2, 5)).await.unwrap(), 1);
}

#[tokio::test]
async fn finished_at_below_the_mark_is_refused() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    f.store
        .record_observation(
            p.run_id,
            &key("AAPL.US"),
            endpoint("ratings"),
            at(1, 20),
            &ok(b"r"),
        )
        .await
        .unwrap();
    for t in [UnixMs(0), at(1, 19)] {
        clock_went_back(
            f.store.end_run(p.run_id, t, Finish::Partial, None).await,
            t,
            at(1, 20),
        );
    }
    f.store
        .end_run(p.run_id, at(1, 20), Finish::Partial, None)
        .await
        .unwrap();
}

/// A config key observed before the freeze, then a freeze at the same
/// instant: both accepted. A freeze before that observation is refused.
#[tokio::test]
async fn frozen_at_below_a_prior_observation_is_refused() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    f.store
        .record_observation(
            p.run_id,
            &key(".SPX.US"),
            endpoint("index_constituents"),
            at(1, 5),
            &ok(&constituents(&["MSFT.US"])),
        )
        .await
        .unwrap();
    f.store
        .record_observation(
            p.run_id,
            &key("AAPL.US"),
            endpoint("ratings"),
            at(1, 9),
            &ok(b"r"),
        )
        .await
        .unwrap();
    clock_went_back(
        f.store.freeze_membership(p.run_id, at(1, 8)).await,
        at(1, 8),
        at(1, 9),
    );
    f.store.freeze_membership(p.run_id, at(1, 9)).await.unwrap();
}

/// The blob and the observation commit together: when the observation
/// insert fails after the blob insert, neither is kept.
#[tokio::test]
async fn blob_and_observation_commit_together() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let count = |sql: &'static str| {
        let path = f.path.clone();
        async move {
            let mut conn = raw(&path, false).await;
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&mut conn)
                .await
                .unwrap()
        }
    };
    // A test-only trigger, added to the file under the open Store, makes
    // the observation insert fail after the blob insert.
    let mut conn = raw(&f.path, false).await;
    sqlx::query(
        "CREATE TRIGGER inject BEFORE INSERT ON observation
         BEGIN SELECT RAISE(ABORT, 'injected'); END",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    let err = f
        .store
        .record_observation(
            p.run_id,
            &key("AAPL.US"),
            endpoint("ratings"),
            at(1, 5),
            &ok(b"fresh body"),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("injected"), "{err}");
    sqlx::query("DROP TRIGGER inject")
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        count("SELECT COUNT(*) FROM blob").await,
        0,
        "no orphan blob"
    );
    assert_eq!(count("SELECT COUNT(*) FROM observation").await, 0);
    let attempt = f
        .store
        .record_observation(
            p.run_id,
            &key("AAPL.US"),
            endpoint("ratings"),
            at(1, 5),
            &ok(b"fresh body"),
        )
        .await
        .unwrap();
    assert_eq!(attempt, 1);
    assert_eq!(count("SELECT COUNT(*) FROM blob").await, 1);
}

/// `incomplete_membership` only from the membership endpoint;
/// `dc_region_restricted` only from an endpoint with a `dc_restrict` (none).
#[tokio::test]
async fn the_error_class_must_fit_the_endpoint() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    let outcome = |class, error_class, code: &str| Outcome::Failed {
        class,
        error: ErrorDetail {
            class: error_class,
            code: code.into(),
        },
        body: None,
    };
    let incomplete = outcome(
        FailureClass::Permanent,
        ErrorClass::IncompleteMembership,
        "zero_total",
    );
    let restricted = outcome(
        FailureClass::RegionRestricted,
        ErrorClass::DcRegionRestricted,
        "dc_region_restricted",
    );
    for (k, e, o) in [
        ("AAPL.US", "consensus", &incomplete),
        ("AAPL.US", "consensus", &restricted),
        (".SPX.US", "index_constituents", &restricted),
    ] {
        let r = f
            .store
            .record_observation(p.run_id, &key(k), endpoint(e), at(1, 5), o)
            .await;
        assert!(
            matches!(r, Err(StoreError::ErrorClassForEndpoint { .. })),
            "{e}: {r:?}"
        );
    }
    f.store
        .record_observation(
            p.run_id,
            &key(".SPX.US"),
            endpoint("index_constituents"),
            at(1, 5),
            &incomplete,
        )
        .await
        .unwrap();
}
