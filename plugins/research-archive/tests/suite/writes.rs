//! The Store's writes: plan and freeze atomicity (I4 at the Store level),
//! idempotent plans, the attempt counter, run ends and missed days.

use research_archive::StoreError;
use research_archive::types::{
    FailureClass, Finish, Membership, Outcome, RunId, RunOutcome, Source, Status, UnixMs,
};

use crate::support::{
    as_of, at, dump, endpoint, fresh, key, keys, observe_and_freeze, ok, plan_request, raw,
    sqlite_message, window,
};

fn failed(class: FailureClass) -> Outcome {
    Outcome::Failed {
        class,
        body: None,
        error_class: Some("envelope".into()),
        error_code: Some("429002".into()),
    }
}

#[tokio::test]
async fn plan_is_idempotent_per_source_and_day() {
    let f = fresh().await;
    let first = f
        .store
        .plan_run(&plan_request(1, &["MSFT.US", "AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    assert!(first.created);
    assert_eq!(first.config, keys(&["AAPL.US", "MSFT.US"]));
    assert_eq!(first.indexes, keys(&[".SPX.US"]));
    assert!(!first.frozen && first.ended.is_none());
    // The config changed since: the stored plan wins and nothing is written.
    let mut changed = plan_request(1, &["NVDA.US"], &[".NDX.US"]);
    changed.started_at = at(1, 99);
    let again = f.store.plan_run(&changed).await.unwrap();
    assert!(!again.created);
    assert_eq!(again, crate::writes::created(false, first.clone()));
    let next = f
        .store
        .plan_run(&plan_request(2, &["NVDA.US"], &[]))
        .await
        .unwrap();
    assert_eq!(next.run_id, RunId(first.run_id.0 + 1));
}

fn created(
    created: bool,
    plan: research_archive::store::RunPlan,
) -> research_archive::store::RunPlan {
    research_archive::store::RunPlan { created, ..plan }
}

#[tokio::test]
async fn plan_aborted_midway_leaves_nothing_and_a_retry_succeeds() {
    let f = fresh().await;
    // The run row and the first key are written before the duplicate key
    // aborts the transaction.
    let bad = plan_request(1, &["AAPL.US", "MSFT.US", "AAPL.US"], &[".SPX.US"]);
    let err = f.store.plan_run(&bad).await.unwrap_err();
    assert!(sqlite_message(&err).contains("run_key row exists"), "{err}");
    f.store.close().await;
    let mut conn = raw(&f.path, false).await;
    for table in ["run", "run_key"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    drop(conn);
    let store = research_archive::Store::open(&f.path, &research_archive::Catalog::compiled())
        .await
        .unwrap();
    let plan = store
        .plan_run(&plan_request(1, &["AAPL.US", "MSFT.US"], &[".SPX.US"]))
        .await
        .unwrap();
    assert!(plan.created);
    assert_eq!(plan.config, keys(&["AAPL.US", "MSFT.US"]));
}

#[tokio::test]
async fn freeze_aborted_midway_leaves_nothing_and_a_retry_succeeds() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".NDX.US", ".SPX.US"]))
        .await
        .unwrap();
    for index in [".NDX.US", ".SPX.US"] {
        f.store
            .record_observation(
                plan.run_id,
                &key(index),
                endpoint("index_constituents"),
                at(1, 5),
                &ok(index.as_bytes()),
            )
            .await
            .unwrap();
    }
    f.store.close().await;
    let before = dump(&mut raw(&f.path, false).await).await;
    let store = research_archive::Store::open(&f.path, &research_archive::Catalog::compiled())
        .await
        .unwrap();
    // `.NDX.US` and its members are written, then `.SPX.US`'s duplicate
    // member aborts the whole freeze.
    let bad = [
        (
            key(".NDX.US"),
            Membership::Observed {
                members: keys(&["AAPL.US", "NVDA.US"]),
            },
        ),
        (
            key(".SPX.US"),
            Membership::Observed {
                members: keys(&["MSFT.US", "MSFT.US"]),
            },
        ),
    ];
    let err = store
        .freeze_membership(plan.run_id, at(1, 6), &bad)
        .await
        .unwrap_err();
    assert!(sqlite_message(&err).contains("run_key row exists"), "{err}");
    assert!(!store.plan(plan.run_id).await.unwrap().frozen);
    store.clone().close().await;
    assert_eq!(dump(&mut raw(&f.path, false).await).await, before);

    let store = research_archive::Store::open(&f.path, &research_archive::Catalog::compiled())
        .await
        .unwrap();
    let good = [
        (bad[0].0.clone(), bad[0].1.clone()),
        (
            key(".SPX.US"),
            Membership::Observed {
                members: keys(&["MSFT.US"]),
            },
        ),
    ];
    store
        .freeze_membership(plan.run_id, at(1, 7), &good)
        .await
        .unwrap();
    assert!(store.plan(plan.run_id).await.unwrap().frozen);
}

#[tokio::test]
async fn a_second_freeze_is_refused_and_writes_nothing() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &[], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(&f.store, &plan, at(1, 5), &[(".SPX.US", &["AAPL.US"])]).await;
    let again = [(
        key(".SPX.US"),
        Membership::Observed {
            members: keys(&["MSFT.US"]),
        },
    )];
    assert!(matches!(
        f.store.freeze_membership(plan.run_id, at(1, 6), &again).await,
        Err(StoreError::AlreadyFrozen(id)) if id == plan.run_id
    ));
    let u = f.store.universe_at(at(1, 100)).await.unwrap().unwrap();
    assert!(u.keys().contains(&key("AAPL.US")) && !u.keys().contains(&key("MSFT.US")));
}

#[tokio::test]
async fn freeze_checks_the_indexes_and_their_sources() {
    let f = fresh().await;
    let day1 = f
        .store
        .plan_run(&plan_request(1, &[], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(&f.store, &day1, at(1, 5), &[(".SPX.US", &["AAPL.US"])]).await;
    let day2 = f
        .store
        .plan_run(&plan_request(2, &[], &[".SPX.US"]))
        .await
        .unwrap();
    let freeze = |m: Membership| [(key(".SPX.US"), m)];
    let observed = Membership::Observed {
        members: keys(&["AAPL.US"]),
    };
    // No ok constituents in this run yet.
    assert!(matches!(
        f.store.freeze_membership(day2.run_id, at(2, 5), &freeze(observed)).await,
        Err(StoreError::NoMembershipObservation { source_run, .. }) if source_run == day2.run_id
    ));
    // A carry must name an earlier run.
    for source in [day2.run_id, RunId(day2.run_id.0 + 1)] {
        let carry = Membership::Carried {
            source,
            members: keys(&["AAPL.US"]),
        };
        assert!(matches!(
            f.store
                .freeze_membership(day2.run_id, at(2, 5), &freeze(carry))
                .await,
            Err(StoreError::CarryNotEarlier { .. })
        ));
    }
    // The index set must be exactly the plan's.
    let wrong = [(key(".NDX.US"), Membership::None)];
    assert!(matches!(
        f.store
            .freeze_membership(day2.run_id, at(2, 5), &wrong)
            .await,
        Err(StoreError::FreezeIndexes { .. })
    ));
    let carried = Membership::Carried {
        source: day1.run_id,
        members: keys(&["AAPL.US"]),
    };
    f.store
        .freeze_membership(day2.run_id, at(2, 5), &freeze(carried))
        .await
        .unwrap();
    f.store.close().await;
    let mut conn = raw(&f.path, false).await;
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT mode, source_run_id FROM run_membership WHERE run_id = ?")
            .bind(day2.run_id.0)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(row, ("carried".to_owned(), Some(day1.run_id.0)));
}

/// `source_run_id` is set if and only if the mode is `carried`, on any
/// connection.
#[tokio::test]
async fn carried_mode_check() {
    let (_dir, path) = crate::support::populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::query("INSERT INTO run VALUES (2, 'longbridge', '2026-01-03', 0, 1, 0)")
        .execute(&mut conn)
        .await
        .unwrap();
    for bad in [
        "(2, '.A.US', 'carried', NULL, 0)",
        "(2, '.B.US', 'observed', 1, 0)",
        "(2, '.C.US', 'none', 1, 0)",
        "(2, '.D.US', 'guessed', NULL, 0)",
    ] {
        let err = sqlx::query(&format!("INSERT INTO run_membership VALUES {bad}"))
            .execute(&mut conn)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "{bad}: {err}"
        );
    }
    for good in [
        "(2, '.A.US', 'carried', 1, 0)",
        "(2, '.B.US', 'observed', NULL, 0)",
        "(2, '.C.US', 'none', NULL, 0)",
    ] {
        sqlx::query(&format!("INSERT INTO run_membership VALUES {good}"))
            .execute(&mut conn)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn closed_sets_are_checked_on_any_connection() {
    let (_dir, path) = crate::support::populated().await;
    let mut conn = raw(&path, false).await;
    for bad in [
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'not_found', NULL, NULL, NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'ok', NULL, NULL, NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 0, 0, 'permanent', NULL, NULL, NULL)",
        "INSERT INTO run_end VALUES (9, 0, 'aborted', NULL)",
        "INSERT INTO run_key VALUES (1, 'X.US', 'index:')",
        "INSERT INTO run_key VALUES (1, 'X.US', 'watchlist')",
        "INSERT INTO endpoint VALUES ('e', 'GET', '/e', '[]', '[]', NULL, 'fund')",
        "INSERT INTO run VALUES (9, 'longbridge', '2026-02-30', 0, 1, 0)",
        "INSERT INTO run VALUES (9, 'longbridge', '2026-03-01', 1, 1, 0)",
        "INSERT INTO run VALUES (9, 'longbridge', '2026-03-01', 'zero', 1, 0)",
        "INSERT INTO blob VALUES ('ABC', X'')",
    ] {
        assert!(sqlx::query(bad).execute(&mut conn).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn attempts_count_per_run_key_and_endpoint() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US", "MSFT.US"], &[]))
        .await
        .unwrap();
    let rec = |k: &'static str, e: &'static str, t: i64, o: Outcome| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(plan.run_id, &key(k), endpoint(e), at(1, t), &o)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        rec("AAPL.US", "consensus", 1, failed(FailureClass::Transient)).await,
        1
    );
    assert_eq!(
        rec("AAPL.US", "consensus", 2, failed(FailureClass::Throttled)).await,
        2
    );
    assert_eq!(rec("AAPL.US", "ratings", 3, ok(b"r")).await, 1);
    assert_eq!(rec("MSFT.US", "consensus", 4, ok(b"m")).await, 1);
    assert_eq!(rec("AAPL.US", "consensus", 5, ok(b"a")).await, 3);
    let rows = f
        .store
        .attempts(plan.run_id, &key("AAPL.US"), endpoint("consensus"))
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            (1, at(1, 1), Status::Failed(FailureClass::Transient)),
            (2, at(1, 2), Status::Failed(FailureClass::Throttled)),
            (3, at(1, 5), Status::Ok),
        ]
    );
    // The next run starts again at 1.
    f.store
        .end_run(plan.run_id, at(1, 9), Finish::Complete, None)
        .await
        .unwrap();
    let day2 = f
        .store
        .plan_run(&plan_request(2, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let n = f
        .store
        .record_observation(
            day2.run_id,
            &key("AAPL.US"),
            endpoint("consensus"),
            at(2, 1),
            &ok(b"a"),
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn observations_must_be_planned_and_the_run_open() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    let rec = |k: &'static str, e: &'static str| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(plan.run_id, &key(k), endpoint(e), at(1, 1), &ok(b"x"))
                .await
        }
    };
    // An unplanned key, an index endpoint for a security, a security endpoint
    // for an index, and a member before the freeze.
    for (k, e) in [
        ("NVDA.US", "consensus"),
        ("AAPL.US", "index_constituents"),
        (".SPX.US", "consensus"),
        ("MSFT.US", "consensus"),
    ] {
        assert!(
            matches!(rec(k, e).await, Err(StoreError::NotPlanned { .. })),
            "{k} {e}"
        );
    }
    observe_and_freeze(&f.store, &plan, at(1, 2), &[(".SPX.US", &["MSFT.US"])]).await;
    rec("MSFT.US", "consensus").await.unwrap();
    f.store
        .end_run(
            plan.run_id,
            at(1, 3),
            Finish::Partial,
            Some("window closed"),
        )
        .await
        .unwrap();
    assert!(matches!(
        rec("AAPL.US", "consensus").await,
        Err(StoreError::RunEnded(_))
    ));
    assert!(matches!(
        f.store
            .end_run(plan.run_id, at(1, 4), Finish::Complete, None)
            .await,
        Err(StoreError::RunEnded(_))
    ));
    assert!(matches!(
        f.store.freeze_membership(plan.run_id, at(1, 4), &[]).await,
        Err(StoreError::RunEnded(_))
    ));
    assert!(matches!(
        f.store
            .end_run(RunId(42), at(1, 4), Finish::Complete, None)
            .await,
        Err(StoreError::UnknownRun(RunId(42)))
    ));
    assert_eq!(
        f.store.plan(plan.run_id).await.unwrap().ended,
        Some(RunOutcome::Partial)
    );
}

#[tokio::test]
async fn dangling_runs_close_as_partial_once_their_window_has_closed() {
    let f = fresh().await;
    let d1 = f
        .store
        .plan_run(&plan_request(1, &["A.US"], &[]))
        .await
        .unwrap();
    let d2 = f
        .store
        .plan_run(&plan_request(2, &["A.US"], &[]))
        .await
        .unwrap();
    let d3 = f
        .store
        .plan_run(&plan_request(3, &["A.US"], &[]))
        .await
        .unwrap();
    f.store
        .end_run(d1.run_id, at(1, 9), Finish::Complete, None)
        .await
        .unwrap();
    // Day 2's window has closed; day 3's has not (its end is the bound).
    let now = window(3).end();
    let closed = f
        .store
        .close_dangling_runs(UnixMs(now.0 - 1))
        .await
        .unwrap();
    assert_eq!(closed, [d2.run_id]);
    assert_eq!(f.store.close_dangling_runs(now).await.unwrap(), [d3.run_id]);
    assert!(f.store.close_dangling_runs(now).await.unwrap().is_empty());
    assert_eq!(
        f.store.plan(d1.run_id).await.unwrap().ended,
        Some(RunOutcome::Complete)
    );
    assert_eq!(
        f.store.plan(d2.run_id).await.unwrap().ended,
        Some(RunOutcome::Partial)
    );
}

#[tokio::test]
async fn missed_days_need_a_closed_window_and_no_run() {
    let f = fresh().await;
    let src = Source::Longbridge;
    assert!(matches!(
        f.store
            .record_missed(src, as_of(1), window(1), at(1, 5))
            .await,
        Err(StoreError::WindowOpen)
    ));
    let missed = f
        .store
        .record_missed(src, as_of(1), window(1), window(1).end())
        .await
        .unwrap();
    let plan = f.store.plan(missed).await.unwrap();
    assert_eq!(plan.ended, Some(RunOutcome::Missed));
    assert!(plan.config.is_empty() && plan.indexes.is_empty());
    assert!(matches!(
        f.store.record_missed(src, as_of(1), window(1), window(1).end()).await,
        Err(StoreError::RunExists(id)) if id == missed
    ));
    // Planning a missed day returns the missed run, unchanged.
    let again = f
        .store
        .plan_run(&plan_request(1, &["A.US"], &[]))
        .await
        .unwrap();
    assert!(!again.created && again.run_id == missed && again.ended == Some(RunOutcome::Missed));
}
