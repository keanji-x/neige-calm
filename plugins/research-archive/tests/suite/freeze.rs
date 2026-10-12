//! The freeze (I4 at the Store level, I5): the Store derives each planned
//! index's mode, source and members from stored `ok` constituents rows.

use research_archive::membership::IncompleteMembership;
use research_archive::store::RunPlan;
use research_archive::types::{
    ErrorClass, ErrorDetail, FailureClass, Membership, Origin, Outcome, RunId, UnixMs,
};
use research_archive::{Store, StoreError};
use sha2::{Digest, Sha256};

use crate::support::{
    at, constituents, dump, endpoint, failed, fresh, key, keys, observe_and_freeze, ok,
    plan_request, raw,
};

async fn plan(store: &Store, n: i64, config: &[&str], indexes: &[&str]) -> RunPlan {
    store
        .plan_run(&plan_request(n, config, indexes))
        .await
        .unwrap()
}

async fn record(store: &Store, run: RunId, index: &str, t: UnixMs, outcome: Outcome) {
    store
        .record_observation(
            run,
            &key(index),
            endpoint("index_constituents"),
            t,
            &outcome,
        )
        .await
        .unwrap();
}

async fn mode_row(path: &std::path::Path, run: RunId) -> Vec<(String, String, Option<i64>)> {
    let mut conn = raw(path, false).await;
    sqlx::query_as(
        "SELECT index_sym, mode, source_run_id FROM run_membership
         WHERE run_id = ? ORDER BY index_sym",
    )
    .bind(run.0)
    .fetch_all(&mut conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn observed_uses_this_runs_latest_ok() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    record(
        &f.store,
        p.run_id,
        ".SPX.US",
        at(1, 5),
        ok(&constituents(&["A.US"])),
    )
    .await;
    record(
        &f.store,
        p.run_id,
        ".SPX.US",
        at(1, 6),
        failed(FailureClass::Transient),
    )
    .await;
    let latest = constituents(&["A.US", "B.US"]);
    record(&f.store, p.run_id, ".SPX.US", at(1, 7), ok(&latest)).await;
    let frozen = f
        .store
        .freeze_membership(p.run_id, at(1, 10))
        .await
        .unwrap();
    assert_eq!(
        frozen,
        [(
            key(".SPX.US"),
            Membership::Observed {
                members: keys(&["A.US", "B.US"])
            }
        )]
    );
    assert_eq!(
        mode_row(&f.path, p.run_id).await,
        [(".SPX.US".into(), "observed".into(), None)]
    );
}

#[tokio::test]
async fn carried_comes_from_the_latest_earlier_ok_and_an_own_ok_wins() {
    let f = fresh().await;
    let d1 = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    observe_and_freeze(&f.store, &d1, at(1, 5), &[(".SPX.US", &["A.US"])]).await;
    let d2 = plan(&f.store, 2, &[], &[".SPX.US"]).await;
    // Day 2 has an earlier ok (day 1) and its own: its own wins.
    let frozen = observe_and_freeze(&f.store, &d2, at(2, 5), &[(".SPX.US", &["B.US"])]).await;
    assert_eq!(frozen[0].1.mode(), "observed");
    let d3 = plan(&f.store, 3, &[], &[".SPX.US"]).await;
    record(
        &f.store,
        d3.run_id,
        ".SPX.US",
        at(3, 4),
        failed(FailureClass::Permanent),
    )
    .await;
    let frozen = f
        .store
        .freeze_membership(d3.run_id, at(3, 5))
        .await
        .unwrap();
    assert_eq!(
        frozen,
        [(
            key(".SPX.US"),
            Membership::Carried {
                source: d2.run_id,
                members: keys(&["B.US"])
            }
        )]
    );
    assert_eq!(
        mode_row(&f.path, d3.run_id).await,
        [(".SPX.US".into(), "carried".into(), Some(d2.run_id.0))]
    );
}

/// Day 2 only carried; day 3's carry names day 1, the run holding the
/// original observation.
#[tokio::test]
async fn a_carry_points_to_the_original_not_an_earlier_carry() {
    let f = fresh().await;
    let d1 = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    observe_and_freeze(&f.store, &d1, at(1, 5), &[(".SPX.US", &["A.US"])]).await;
    let d2 = plan(&f.store, 2, &[], &[".SPX.US"]).await;
    let frozen = f
        .store
        .freeze_membership(d2.run_id, at(2, 5))
        .await
        .unwrap();
    assert!(matches!(&frozen[0].1, Membership::Carried { source, .. } if *source == d1.run_id));
    let d3 = plan(&f.store, 3, &[], &[".SPX.US"]).await;
    let frozen = f
        .store
        .freeze_membership(d3.run_id, at(3, 5))
        .await
        .unwrap();
    assert_eq!(
        frozen[0].1,
        Membership::Carried {
            source: d1.run_id,
            members: keys(&["A.US"])
        }
    );
}

/// `none` only when no run has an ok constituents row for that index: a
/// failure (even with a body) or another index's ok does not count.
#[tokio::test]
async fn none_only_when_no_ok_exists_anywhere() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &[], &[".NDX.US", ".SPX.US"]).await;
    record(
        &f.store,
        p.run_id,
        ".NDX.US",
        at(1, 2),
        ok(&constituents(&["A.US"])),
    )
    .await;
    let incomplete = br#"{"total":503,"stocks":[{"symbol":"A.US"}]}"#.to_vec();
    let filed = Outcome::Failed {
        class: FailureClass::Permanent,
        error: ErrorDetail {
            class: ErrorClass::IncompleteMembership,
            code: "count_mismatch".into(),
        },
        body: Some(incomplete),
    };
    record(&f.store, p.run_id, ".SPX.US", at(1, 3), filed).await;
    let frozen = f.store.freeze_membership(p.run_id, at(1, 5)).await.unwrap();
    assert_eq!(
        frozen,
        [
            (
                key(".NDX.US"),
                Membership::Observed {
                    members: keys(&["A.US"])
                }
            ),
            (key(".SPX.US"), Membership::None),
        ]
    );
    let u = f.store.universe_at(at(1, 5)).await.unwrap().unwrap();
    assert!(u.pending.is_empty());
    assert!(
        !u.members
            .iter()
            .any(|(_, o)| *o == Origin::Index(key(".SPX.US")))
    );
}

/// I5: a two-index carry with an overlapping member succeeds, and the
/// member keeps every origin.
#[tokio::test]
async fn i5_a_two_index_carry_with_an_overlapping_member_succeeds() {
    let f = fresh().await;
    let d1 = plan(&f.store, 1, &[], &[".NDX.US", ".SPX.US"]).await;
    let both: &[(&str, &[&str])] = &[
        (".NDX.US", &["AAPL.US"]),
        (".SPX.US", &["AAPL.US", "MSFT.US"]),
    ];
    observe_and_freeze(&f.store, &d1, at(1, 5), both).await;
    let d2 = plan(&f.store, 2, &["AAPL.US"], &[".NDX.US", ".SPX.US"]).await;
    let frozen = f
        .store
        .freeze_membership(d2.run_id, at(2, 5))
        .await
        .unwrap();
    assert!(
        frozen
            .iter()
            .all(|(_, m)| matches!(m, Membership::Carried { source, .. } if *source == d1.run_id))
    );
    let u = f.store.universe_at(at(2, 5)).await.unwrap().unwrap();
    assert_eq!(u.run_id, d2.run_id);
    let aapl: Vec<_> = u
        .members
        .iter()
        .filter(|(k, _)| *k == key("AAPL.US"))
        .collect();
    assert_eq!(aapl.len(), 3, "config, index:.NDX.US and index:.SPX.US");
}

/// An incomplete body is never an `ok` membership row: `record_observation`
/// refuses it, and a row written around the Store refuses the freeze.
#[tokio::test]
async fn incomplete_ok_bodies_are_refused_at_record_and_at_freeze() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    let zero = br#"{"total":0,"stocks":[]}"#;
    let err = f
        .store
        .record_observation(
            p.run_id,
            &key(".SPX.US"),
            endpoint("index_constituents"),
            at(1, 2),
            &ok(zero),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::IncompleteMembership {
            reason: IncompleteMembership::ZeroTotal,
            ..
        }
    ));
    raw_ok_row(&f.path, p.run_id, ".SPX.US", 1, at(1, 3), zero).await;
    let err = f
        .store
        .freeze_membership(p.run_id, at(1, 5))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            StoreError::StoredMembershipInvalid {
                attempt: 1,
                reason: IncompleteMembership::ZeroTotal,
                ..
            }
        ),
        "{err}"
    );
}

/// Inserts an `ok` constituents row with `body` on a raw connection, as a
/// writer that bypasses `record_observation` would.
async fn raw_ok_row(
    path: &std::path::Path,
    run: RunId,
    index: &str,
    attempt: i64,
    t: UnixMs,
    body: &[u8],
) {
    let sha = hex::encode(Sha256::digest(body));
    let mut conn = raw(path, false).await;
    sqlx::query("INSERT INTO blob VALUES (?, ?)")
        .bind(&sha)
        .bind(body)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO observation VALUES (?, ?, 'index_constituents', ?, ?, 'ok', ?, NULL, NULL)",
    )
    .bind(run.0)
    .bind(index)
    .bind(attempt)
    .bind(t.0)
    .bind(&sha)
    .execute(&mut conn)
    .await
    .unwrap();
}

#[tokio::test]
async fn freeze_aborted_midway_leaves_nothing_and_a_retry_succeeds() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &["AAPL.US"], &[".NDX.US", ".SPX.US"]).await;
    record(
        &f.store,
        p.run_id,
        ".NDX.US",
        at(1, 2),
        ok(&constituents(&["AAPL.US", "NVDA.US"])),
    )
    .await;
    // `.NDX.US` sorts first: its membership row and members are written,
    // then `.SPX.US`'s malformed body aborts the whole freeze.
    raw_ok_row(&f.path, p.run_id, ".SPX.US", 1, at(1, 3), b"not json").await;
    let before = dump(&mut raw(&f.path, false).await).await;
    let err = f
        .store
        .freeze_membership(p.run_id, at(1, 5))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::StoredMembershipInvalid { .. }),
        "{err}"
    );
    assert_eq!(dump(&mut raw(&f.path, false).await).await, before);
    assert!(!f.store.plan(p.run_id).await.unwrap().frozen);
    // A later complete observation is the latest ok; the retry succeeds.
    record(
        &f.store,
        p.run_id,
        ".SPX.US",
        at(1, 6),
        ok(&constituents(&["MSFT.US"])),
    )
    .await;
    let frozen = f.store.freeze_membership(p.run_id, at(1, 7)).await.unwrap();
    assert_eq!(frozen.len(), 2);
    assert!(f.store.plan(p.run_id).await.unwrap().frozen);
}

/// No look-ahead: a freeze cannot use a body fetched after it, and it must
/// fall inside the run's window.
#[tokio::test]
async fn freeze_time_is_checked() {
    let f = fresh().await;
    let d1 = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    record(
        &f.store,
        d1.run_id,
        ".SPX.US",
        at(1, 20),
        ok(&constituents(&["A.US"])),
    )
    .await;
    assert!(matches!(
        f.store.freeze_membership(d1.run_id, at(1, 10)).await,
        Err(StoreError::FreezeBeforeObservation { fetched_at, .. }) if fetched_at == at(1, 20)
    ));
    let end = crate::support::window(1).end();
    assert!(matches!(
        f.store.freeze_membership(d1.run_id, end).await,
        Err(StoreError::OutsideWindow { at, .. }) if at == end
    ));
    f.store
        .freeze_membership(d1.run_id, at(1, 20))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_second_freeze_is_refused_and_a_late_ok_changes_nothing() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &[], &[".SPX.US"]).await;
    observe_and_freeze(&f.store, &p, at(1, 5), &[(".SPX.US", &["AAPL.US"])]).await;
    // A constituents fetch after the freeze is recorded but changes nothing.
    record(
        &f.store,
        p.run_id,
        ".SPX.US",
        at(1, 6),
        ok(&constituents(&["MSFT.US"])),
    )
    .await;
    assert!(matches!(
        f.store.freeze_membership(p.run_id, at(1, 7)).await,
        Err(StoreError::AlreadyFrozen(id)) if id == p.run_id
    ));
    let u = f.store.universe_at(at(1, 100)).await.unwrap().unwrap();
    assert!(u.keys().contains(&key("AAPL.US")) && !u.keys().contains(&key("MSFT.US")));
}

#[tokio::test]
async fn a_run_without_indexes_has_nothing_to_freeze() {
    let f = fresh().await;
    let p = plan(&f.store, 1, &["AAPL.US"], &[]).await;
    assert!(
        f.store
            .freeze_membership(p.run_id, at(1, 5))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(mode_row(&f.path, p.run_id).await.is_empty());
}
