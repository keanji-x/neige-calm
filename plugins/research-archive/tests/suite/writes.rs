//! The Store's writes: plan atomicity (I4 at the Store level), idempotent
//! plans, the window bound, the attempt counter, run ends and missed days.
//! The freeze has its own module.

use research_archive::StoreError;
use research_archive::types::{
    FailureClass, Finish, Response, RunId, RunOutcome, Source, Status, UnixMs,
};

use crate::support::{
    as_of, at, constituents, endpoint, failed, fresh, key, keys, observe_and_freeze, ok,
    plan_request, raw, sqlite_message, window,
};

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
    let store = research_archive::Store::open(&f.path).await.unwrap();
    let plan = store
        .plan_run(&plan_request(1, &["AAPL.US", "MSFT.US"], &[".SPX.US"]))
        .await
        .unwrap();
    assert!(plan.created);
    assert_eq!(plan.config, keys(&["AAPL.US", "MSFT.US"]));
}

/// `source_run_id` is set if and only if the mode is `carried`, also on a
/// raw connection with default check settings.
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
async fn closed_sets_are_checked_with_default_settings() {
    let (_dir, path) = crate::support::populated().await;
    let mut conn = raw(&path, false).await;
    for bad in [
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'not_found', NULL, NULL, NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'ok', NULL, NULL, NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 0, 0, 'permanent', NULL, 'http_status', '404')",
        // A failure needs its class and code; ok has neither.
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, NULL, NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, 'http_status', NULL)",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, 'http_status', '')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, 'teapot', 'x')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, 'http_status', 'Not Found')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'transient', NULL, 'incomplete_membership', 'zero_total')",
        // Bytes only for ok and for an incomplete membership body.
        "INSERT INTO observation VALUES (1, '.SPX.US', 'index_constituents', 9, 0, 'permanent', NULL, 'incomplete_membership', 'zero_total')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'transient', (SELECT MIN(sha256) FROM blob), 'transport', 'x')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'permanent', NULL, 'dc_region_restricted', 'x')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'region_restricted', NULL, 'auth', 'x')",
        "INSERT INTO observation VALUES (1, 'AAPL.US', 'consensus', 9, 0, 'ok', (SELECT MIN(sha256) FROM blob), 'envelope', '1')",
        "INSERT INTO run_end VALUES (9, 0, 'aborted', NULL)",
        "INSERT INTO run_key VALUES (1, 'X.US', 'index:')",
        "INSERT INTO run_key VALUES (1, 'X.US', 'watchlist')",
        "INSERT INTO endpoint VALUES ('e', 'GET', '/e', '[]', '[]', NULL, 'fund')",
        "INSERT INTO run VALUES (9, 'longbridge', '2026-02-30', 0, 1, 0)",
        // `date()` of an impossible day is NULL, which alone would pass a CHECK.
        "INSERT INTO run VALUES (9, 'longbridge', '2026-99-99', 0, 1, 0)",
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
    let rec = |k: &'static str, e: &'static str, t: i64, o: Response| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(plan.run_id, &key(k), endpoint(e), at(1, t), &o)
                .await
                .unwrap()
                .attempt
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
        .end_run(plan.run_id, at(1, 9), Finish::Partial, None)
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
    assert_eq!(n.attempt, 1);
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
        let body = if e == "index_constituents" {
            constituents(&["A.US"])
        } else {
            b"x".to_vec()
        };
        async move {
            store
                .record_observation(plan.run_id, &key(k), endpoint(e), at(1, 1), &ok(&body))
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
    // MSFT.US became planned at the freeze (t = 2): an observation stamped
    // before it is refused, so no value predates the membership behind it.
    assert!(matches!(
        rec("MSFT.US", "consensus").await,
        Err(StoreError::ClockWentBack { at: t, mark, .. }) if t == at(1, 1) && mark == at(1, 2)
    ));
    f.store
        .record_observation(
            plan.run_id,
            &key("MSFT.US"),
            endpoint("consensus"),
            at(1, 2),
            &ok(b"x"),
        )
        .await
        .unwrap();
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
        f.store.freeze_membership(plan.run_id, at(1, 4)).await,
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
        .end_run(d1.run_id, at(1, 9), Finish::Partial, None)
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
        Some(RunOutcome::Partial)
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

/// I1 at the single owner: no row outside its run's window.
#[tokio::test]
async fn rows_outside_the_window_are_refused() {
    let f = fresh().await;
    let mut early = plan_request(1, &["AAPL.US"], &[]);
    early.started_at = UnixMs(window(1).start().0 - 1);
    assert!(matches!(
        f.store.plan_run(&early).await,
        Err(StoreError::PlanOutsideWindow { .. })
    ));
    // Planned at the window's first instant, so both bounds are reachable.
    let mut at_start = plan_request(1, &["AAPL.US"], &[]);
    at_start.started_at = window(1).start();
    let plan = f.store.plan_run(&at_start).await.unwrap();
    let rec = |t: UnixMs| {
        let store = f.store.clone();
        async move {
            store
                .record_observation(
                    plan.run_id,
                    &key("AAPL.US"),
                    endpoint("ratings"),
                    t,
                    &ok(b"r"),
                )
                .await
        }
    };
    for t in [UnixMs(window(1).start().0 - 1), window(1).end()] {
        assert!(
            matches!(rec(t).await, Err(StoreError::OutsideWindow { at, .. }) if at == t),
            "{t:?}"
        );
    }
    assert_eq!(rec(window(1).start()).await.unwrap().attempt, 1);
    assert_eq!(rec(UnixMs(window(1).end().0 - 1)).await.unwrap().attempt, 2);
}

/// `complete` needs every planned index frozen and every planned pair with
/// a row; `partial` needs nothing.
#[tokio::test]
async fn complete_needs_a_frozen_universe_and_a_row_per_pair() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    let complete = || {
        let store = f.store.clone();
        async move {
            store
                .end_run(plan.run_id, at(1, 500), Finish::Complete, None)
                .await
        }
    };
    assert!(matches!(
        complete().await,
        Err(StoreError::NotComplete {
            frozen: false,
            pairs_without_rows,
            ..
        }) if pairs_without_rows == crate::archive::security_count() + 7
    ));
    observe_and_freeze(&f.store, &plan, at(1, 10), &[(".SPX.US", &["MSFT.US"])]).await;
    // AAPL.US and MSFT.US × 6 security endpoints are still empty.
    assert!(matches!(
        complete().await,
        Err(StoreError::NotComplete {
            frozen: true,
            pairs_without_rows,
            ..
        }) if pairs_without_rows == 2 * crate::archive::security_count() + 6
    ));
    for pair in f
        .store
        .coverage(plan.run_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|p| !p.has_ok)
    {
        f.store
            .record_observation(
                plan.run_id,
                &pair.key,
                pair.endpoint,
                at(1, 20),
                &failed(FailureClass::Permanent),
            )
            .await
            .unwrap();
    }
    complete().await.unwrap();
    assert_eq!(
        f.store.plan(plan.run_id).await.unwrap().ended,
        Some(RunOutcome::Complete)
    );
}

/// Two Stores on one file, writing one pair at once with a real increasing
/// clock: each task reads the clock, waits a varying "latency", then
/// records. `BEGIN IMMEDIATE` and the busy timeout serialize the writes; a
/// write stamped before the run's clock is refused with `ClockWentBack`.
/// Every accepted row's attempt order is its time order, with no gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_keep_attempt_order_equal_to_time_order() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let other = research_archive::Store::open(&f.path).await.unwrap();
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(100));
    let mut tasks = Vec::new();
    for i in 0..20u64 {
        let store = if i % 2 == 0 {
            f.store.clone()
        } else {
            other.clone()
        };
        let clock = clock.clone();
        tasks.push(tokio::spawn(async move {
            let t = at(1, clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            tokio::time::sleep(std::time::Duration::from_millis((i % 4) * 15)).await;
            let result = store
                .record_observation(
                    plan.run_id,
                    &key("AAPL.US"),
                    endpoint("consensus"),
                    t,
                    &failed(FailureClass::Throttled),
                )
                .await;
            (t, result)
        }));
    }
    let mut accepted = Vec::new();
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            (t, Ok(recorded)) => accepted.push((recorded.attempt, t)),
            (t, Err(StoreError::ClockWentBack { at, mark, .. })) => {
                assert!(at == t && mark > t);
                refused += 1;
            }
            (_, Err(other)) => panic!("{other}"),
        }
    }
    accepted.sort();
    let attempts: Vec<u32> = accepted.iter().map(|(a, _)| *a).collect();
    assert_eq!(attempts, (1..=accepted.len() as u32).collect::<Vec<_>>());
    assert!(
        accepted.windows(2).all(|w| w[0].1 <= w[1].1),
        "{accepted:?}"
    );
    assert!(refused > 0, "the varying latency must reorder some writes");
    let rows = f
        .store
        .attempts(plan.run_id, &key("AAPL.US"), endpoint("consensus"))
        .await
        .unwrap();
    let stored: Vec<(u32, UnixMs)> = rows.iter().map(|(a, t, _)| (*a, *t)).collect();
    assert_eq!(stored, accepted);
    other.close().await;
}

/// Windows of one source never overlap and follow `as_of` order, so across
/// runs `fetched_at` order is `as_of` order. Touching windows are fine.
#[tokio::test]
async fn windows_never_overlap_and_follow_as_of() {
    let f = fresh().await;
    let d1 = f
        .store
        .plan_run(&plan_request(1, &["A.US"], &[]))
        .await
        .unwrap();
    let request = |n, start: i64, end: i64| {
        let mut r = plan_request(n, &["A.US"], &[]);
        r.window = research_archive::types::Window::new(UnixMs(start), UnixMs(end)).unwrap();
        r.started_at = UnixMs(start);
        r
    };
    let w1 = window(1);
    for bad in [
        // A later day overlapping day 1's end.
        request(2, w1.end().0 - 1, w1.end().0 + 100),
        // A later day placed before day 1.
        request(2, w1.start().0 - 200, w1.start().0 - 100),
        // An earlier day overlapping day 1's start.
        request(0, w1.start().0 - 100, w1.start().0 + 1),
    ] {
        assert!(
            matches!(
                f.store.plan_run(&bad).await,
                Err(StoreError::WindowConflict { with }) if with == d1.run_id
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        f.store
            .record_missed(
                Source::Longbridge,
                as_of(2),
                research_archive::types::Window::new(UnixMs(w1.end().0 - 1), UnixMs(w1.end().0 + 100))
                    .unwrap(),
                UnixMs(w1.end().0 + 100),
            )
            .await,
        Err(StoreError::WindowConflict { with }) if with == d1.run_id
    ));
    // Touching windows on both sides.
    f.store
        .plan_run(&request(2, w1.end().0, w1.end().0 + 100))
        .await
        .unwrap();
    f.store
        .record_missed(
            Source::Longbridge,
            as_of(0),
            research_archive::types::Window::new(UnixMs(w1.start().0 - 100), w1.start()).unwrap(),
            w1.start(),
        )
        .await
        .unwrap();
}
