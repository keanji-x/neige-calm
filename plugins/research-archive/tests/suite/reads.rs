//! The read contract: the point-in-time value (I6), overlapping origins
//! (I5), the universe at a time, and coverage.

use crate::archive::security_count;
use research_archive::types::{FailureClass, Finish, Origin, Response, UnixMs};

use crate::support::{
    as_of, at, constituents, endpoint, failed, fresh, key, observe_and_freeze, ok, plan_request,
};

fn permanent() -> Response {
    failed(FailureClass::Permanent)
}

/// Day 1 ok with a full body, day 2 `permanent`, day 3 ok with `body`.
async fn three_days(body: &[u8]) -> crate::support::Fixture {
    let f = fresh().await;
    let outcomes = [ok(b"{\"eps\":[1.25,2.5]}"), permanent(), ok(body)];
    for (n, outcome) in (1..=3).zip(outcomes) {
        let plan = f
            .store
            .plan_run(&plan_request(n, &["AAPL.US"], &[]))
            .await
            .unwrap();
        f.store
            .record_observation(
                plan.run_id,
                &key("AAPL.US"),
                endpoint("consensus"),
                at(n, 100),
                &outcome,
            )
            .await
            .unwrap();
        f.store
            .end_run(plan.run_id, at(n, 200), Finish::Partial, None)
            .await
            .unwrap();
    }
    f
}

async fn body_at(f: &crate::support::Fixture, t: UnixMs) -> Option<(Vec<u8>, UnixMs, String)> {
    f.store
        .read_at(&key("AAPL.US"), endpoint("consensus"), t)
        .await
        .unwrap()
        .map(|r| (r.body, r.fetched_at, r.as_of.to_string()))
}

#[tokio::test]
async fn i6_failures_never_win_and_a_later_empty_body_does() {
    // Zero bytes and an empty-shaped body are both values.
    for empty in [&b""[..], &b"{\"list\":[]}"[..]] {
        let f = three_days(empty).await;
        let day1 = (
            b"{\"eps\":[1.25,2.5]}".to_vec(),
            at(1, 100),
            "2026-01-02".to_owned(),
        );
        assert_eq!(body_at(&f, at(1, 99)).await, None, "before the first fetch");
        assert_eq!(body_at(&f, at(1, 100)).await, Some(day1.clone()));
        assert_eq!(
            body_at(&f, at(2, 500)).await,
            Some(day1.clone()),
            "permanent never wins"
        );
        assert_eq!(
            body_at(&f, at(3, 100)).await,
            Some((empty.to_vec(), at(3, 100), "2026-01-04".to_owned())),
            "the empty body is the latest value"
        );
        // A row fetched after t is invisible.
        assert_eq!(body_at(&f, at(3, 99)).await, Some(day1));
    }
}

#[tokio::test]
async fn i6_every_failure_class_is_skipped() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let rec = |t: i64, o: Response| {
        let store = f.store.clone();
        let run = plan.run_id;
        async move {
            store
                .record_observation(run, &key("AAPL.US"), endpoint("ratings"), at(1, t), &o)
                .await
                .unwrap()
        }
    };
    rec(1, ok(b"first")).await;
    for (t, class) in [
        (2, FailureClass::Transient),
        (3, FailureClass::Throttled),
        (4, FailureClass::AuthUnavailable),
        // `region_restricted` needs an endpoint with a `dc_restrict`; the
        // catalog has none.
        (6, FailureClass::Permanent),
    ] {
        rec(t, failed(class)).await;
    }
    let read = f
        .store
        .read_at(&key("AAPL.US"), endpoint("ratings"), at(1, 1_000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((read.body, read.fetched_at), (b"first".to_vec(), at(1, 1)));
    // Fail, retry, succeed: every attempt is a row and the latest ok wins.
    rec(7, ok(b"second")).await;
    let read = f
        .store
        .read_at(&key("AAPL.US"), endpoint("ratings"), at(1, 1_000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.body, b"second");
}

#[tokio::test]
async fn identical_bodies_share_one_blob() {
    let f = fresh().await;
    for n in 1..=2 {
        let plan = f
            .store
            .plan_run(&plan_request(n, &["AAPL.US"], &[]))
            .await
            .unwrap();
        f.store
            .record_observation(
                plan.run_id,
                &key("AAPL.US"),
                endpoint("valuation"),
                at(n, 1),
                &ok(b"same"),
            )
            .await
            .unwrap();
    }
    f.store.clone().close().await;
    let mut conn = crate::support::raw(&f.path, false).await;
    let blobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM observation")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!((blobs, rows), (1, 2));
}

#[tokio::test]
async fn i5_overlapping_origins_are_kept() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(
            1,
            &["AAPL.US", "TSM.US"],
            &[".NDX.US", ".SPX.US"],
        ))
        .await
        .unwrap();
    // AAPL.US is configured and a member of both indexes.
    observe_and_freeze(
        &f.store,
        &plan,
        at(1, 50),
        &[
            (".NDX.US", &["AAPL.US", "NVDA.US"]),
            (".SPX.US", &["AAPL.US", "MSFT.US"]),
        ],
    )
    .await;
    let u = f.store.universe_at(at(1, 50)).await.unwrap().unwrap();
    let ndx = || Origin::Index(key(".NDX.US"));
    let spx = || Origin::Index(key(".SPX.US"));
    assert_eq!(
        u.members,
        [
            (key(".NDX.US"), Origin::IndexSymbol),
            (key(".SPX.US"), Origin::IndexSymbol),
            (key("AAPL.US"), Origin::Config),
            (key("AAPL.US"), ndx()),
            (key("AAPL.US"), spx()),
            (key("MSFT.US"), spx()),
            (key("NVDA.US"), ndx()),
            (key("TSM.US"), Origin::Config),
        ]
    );
    assert!(u.pending.is_empty());
    // Each distinct key counts once.
    assert_eq!(u.keys().len(), 6);
    let coverage = f.store.coverage(plan.run_id).await.unwrap();
    // 4 securities × 6 endpoints + 2 indexes × 1.
    assert_eq!(coverage.len(), 4 * security_count() + 2);
    let aapl = coverage.iter().filter(|c| c.key == key("AAPL.US")).count();
    assert_eq!(aapl, security_count());
}

#[tokio::test]
async fn universe_at_follows_started_at_and_frozen_at() {
    let f = fresh().await;
    assert_eq!(f.store.universe_at(at(5, 0)).await.unwrap(), None);
    let day1 = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(&f.store, &day1, at(1, 10), &[(".SPX.US", &["MSFT.US"])]).await;
    f.store
        .end_run(day1.run_id, at(1, 20), Finish::Partial, None)
        .await
        .unwrap();
    let day2 = f
        .store
        .plan_run(&plan_request(2, &["NVDA.US"], &[".SPX.US"]))
        .await
        .unwrap();
    // Day 2 is planned at at(2, 1) but not frozen until at(2, 10).
    f.store
        .record_observation(
            day2.run_id,
            &key(".SPX.US"),
            endpoint("index_constituents"),
            at(2, 9),
            &ok(&constituents(&["TSLA.US"])),
        )
        .await
        .unwrap();
    f.store
        .freeze_membership(day2.run_id, at(2, 10))
        .await
        .unwrap();
    // A missed day after day 2 does not hide it.
    f.store
        .record_missed(
            research_archive::types::Source::Longbridge,
            crate::support::as_of(3),
            crate::support::window(3),
            at(4, 0),
        )
        .await
        .unwrap();

    let keys_at = |t| {
        let store = f.store.clone();
        async move {
            let u = store.universe_at(t).await.unwrap().unwrap();
            let keys: Vec<String> = u.keys().iter().map(|k| k.to_string()).collect();
            let pending: Vec<String> = u.pending.iter().map(|k| k.to_string()).collect();
            (u.run_id, keys, pending)
        }
    };
    assert_eq!(
        f.store.universe_at(at(1, 0)).await.unwrap(),
        None,
        "before day 1's plan"
    );
    assert_eq!(
        keys_at(at(1, 9)).await,
        (
            day1.run_id,
            vec![".SPX.US".into(), "AAPL.US".into()],
            vec![".SPX.US".into()]
        )
    );
    assert_eq!(
        keys_at(at(1, 10)).await,
        (
            day1.run_id,
            vec![".SPX.US".into(), "AAPL.US".into(), "MSFT.US".into()],
            vec![]
        )
    );
    assert_eq!(
        keys_at(at(2, 9)).await,
        (
            day2.run_id,
            vec![".SPX.US".into(), "NVDA.US".into()],
            vec![".SPX.US".into()]
        )
    );
    assert_eq!(
        keys_at(at(5, 0)).await,
        (
            day2.run_id,
            vec![".SPX.US".into(), "NVDA.US".into(), "TSLA.US".into()],
            vec![]
        )
    );
}

#[tokio::test]
async fn coverage_counts_rows_per_planned_pair() {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    let before = f.store.coverage(plan.run_id).await.unwrap();
    assert_eq!(before.len(), security_count() + 1);
    assert!(before.iter().all(|c| c.attempts == 0 && !c.has_ok));
    f.store
        .record_observation(
            plan.run_id,
            &key("AAPL.US"),
            endpoint("consensus"),
            at(1, 2),
            &permanent(),
        )
        .await
        .unwrap();
    observe_and_freeze(&f.store, &plan, at(1, 3), &[(".SPX.US", &["MSFT.US"])]).await;
    let after = f.store.coverage(plan.run_id).await.unwrap();
    assert_eq!(
        after.len(),
        2 * security_count() + 1,
        "members join the coverage at the freeze"
    );
    let get = |k: &str, e: &str| {
        after
            .iter()
            .find(|c| c.key == key(k) && c.endpoint == endpoint(e))
            .map(|c| (c.attempts, c.has_ok))
            .unwrap()
    };
    assert_eq!(get("AAPL.US", "consensus"), (1, false));
    assert_eq!(get(".SPX.US", "index_constituents"), (1, true));
    assert_eq!(get("MSFT.US", "valuation"), (0, false));
}

/// Equal `fetched_at` within a run (the clock allows equal times): the
/// higher attempt wins. Across runs of a source equal times cannot occur:
/// windows do not overlap, so `fetched_at` order is `as_of` order.
#[tokio::test]
async fn read_at_ties_go_to_the_higher_attempt() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let t = at(1, 50);
    for body in [&b"attempt1"[..], b"attempt2"] {
        f.store
            .record_observation(p.run_id, &key("AAPL.US"), endpoint("ratings"), t, &ok(body))
            .await
            .unwrap();
    }
    let read = f
        .store
        .read_at(&key("AAPL.US"), endpoint("ratings"), t)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.body, b"attempt2");
}

/// Across runs the latest day wins, even when run ids do not follow `as_of`
/// (day 2 planned before day 1).
#[tokio::test]
async fn read_at_across_runs_follows_the_days() {
    let f = fresh().await;
    let d2 = f
        .store
        .plan_run(&plan_request(2, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let d1 = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    assert!(d2.run_id < d1.run_id);
    for (run, n, body) in [(d1.run_id, 1, &b"day1"[..]), (d2.run_id, 2, b"day2")] {
        f.store
            .record_observation(
                run,
                &key("AAPL.US"),
                endpoint("ratings"),
                at(n, 5),
                &ok(body),
            )
            .await
            .unwrap();
    }
    let read = |t| {
        let store = f.store.clone();
        async move {
            let r = store
                .read_at(&key("AAPL.US"), endpoint("ratings"), t)
                .await
                .unwrap()
                .unwrap();
            (r.body, r.as_of)
        }
    };
    assert_eq!(read(at(1, 999)).await, (b"day1".to_vec(), as_of(1)));
    assert_eq!(read(at(3, 0)).await, (b"day2".to_vec(), as_of(2)));
}

/// A kept `incomplete_membership` body has bytes but is not a value:
/// `read_at` returns the earlier `ok` body, not the later incomplete one.
#[tokio::test]
async fn read_at_skips_a_kept_incomplete_membership_body() {
    let f = fresh().await;
    let p = f
        .store
        .plan_run(&plan_request(1, &[], &[".SPX.US"]))
        .await
        .unwrap();
    let complete = constituents(&["AAPL.US"]);
    let incomplete = br#"{"total":2,"stocks":[{"symbol":"AAPL.US"}]}"#.to_vec();
    for (t, body) in [(5, &complete), (10, &incomplete)] {
        f.store
            .record_observation(
                p.run_id,
                &key(".SPX.US"),
                endpoint("index_constituents"),
                at(1, t),
                &ok(body),
            )
            .await
            .unwrap();
    }
    let read = f
        .store
        .read_at(&key(".SPX.US"), endpoint("index_constituents"), at(1, 20))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((read.body, read.fetched_at), (complete, at(1, 5)));
}
