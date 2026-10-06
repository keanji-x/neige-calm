//! #2251 — the launchpad `ensure` decides whether the Planner needs a start under the card's
//! start fence, after any in-flight start has settled: two concurrent first ensures start the
//! Planner once, and a failed keyed re-point does not wedge every later ensure on its key.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::db::sqlite::SqlxRepo;
use serde_json::{Value, json};

use crate::planner_first_start::{AppServer, app_state, router};
use crate::planner_repoint_restart_lock::{joined, request, wait_until};

fn ensure(app: axum::Router) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    tokio::spawn(request(
        app,
        "POST",
        "/api/today/launchpad/ensure".into(),
        json!({}),
    ))
}

async fn scalar(repo: &SqlxRepo, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(repo.pool())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

/// Every start this database holds; the launchpad's Planner is the only harness card here.
async fn start_ops(repo: &SqlxRepo) -> i64 {
    scalar(
        repo,
        "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start'",
    )
    .await
}

async fn launchpad_planner(repo: &SqlxRepo) -> String {
    sqlx::query_scalar(
        "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
          WHERE t.purpose = 'launchpad' AND c.role = 'planner'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

/// The second ensure commits its transaction while the first's bootstrap is still held in
/// `thread/start`, so its own durable read says "nothing started at this path yet". It must
/// re-decide once the bootstrap has settled and start nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_first_ensures_start_the_planner_once() {
    let (_tmp, repo, state) = app_state(AppServer::Running).await;
    let app = router(&state);

    let thread_start = state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let first = ensure(app.clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    while start_ops(&repo).await == 0 {
        assert!(
            Instant::now() < deadline,
            "the first ensure never submitted its bootstrap"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let card_id = launchpad_planner(&repo).await;
    let held = state.planner_recovery_lock_handles_for_test(&card_id);
    assert!(held > 0, "premise: the bootstrap holds the card's lock");

    let second = ensure(app.clone());
    wait_until(
        "the second ensure committed and queues on the card's lock",
        || state.planner_recovery_lock_handles_for_test(&card_id) > held,
    )
    .await;
    assert!(
        !first.is_finished(),
        "premise: the bootstrap is in thread/start"
    );

    drop(thread_start);
    let (status, first) = joined(first, "the first ensure finishes").await;
    assert_eq!(status, StatusCode::CREATED, "body={first}");
    let (status, second) = joined(second, "the second ensure finishes").await;
    assert_eq!(status, StatusCode::OK, "body={second}");
    assert_eq!(first, second, "both ensures answer the one launchpad");

    assert_eq!(
        start_ops(&repo).await,
        1,
        "the Planner was started more than once by two first ensures"
    );
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 \
           AND state IN ('starting','running','idle','turn_pending')",
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active, 1, "the Planner has one live session");
}

/// A keyed re-point whose `thread/start` failed is a permanent `failed` row under its key. The
/// next ensure at the same path must start again under a stepped key, not replay that failure.
#[tokio::test]
async fn a_failed_repoint_does_not_wedge_later_ensures() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let (status, body) = joined(ensure(router(&state)), "the bootstrap ensure").await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // The same server over a moved workspace root: the next ensure re-points the card.
    let state = state.with_workspace_root(tmp.path().join("workspaces-moved"));
    let app = router(&state);
    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = joined(ensure(app.clone()), "the failing re-point").await;
    assert!(
        status.is_server_error(),
        "premise: the re-point's thread/start failed; status={status} body={body}"
    );
    assert_eq!(
        scalar(
            &repo,
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND idempotency_key LIKE 'today-launchpad:%:repoint:%' AND phase = 'failed'",
        )
        .await,
        1,
        "premise: the failed re-point left its keyed row behind"
    );

    let (status, body) = joined(ensure(app.clone()), "the ensure after the failure").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the failed re-point's key wedged the launchpad; body={body}"
    );
    let starts = start_ops(&repo).await;
    assert_eq!(starts, 3, "bootstrap, the failed re-point and its retry");

    // Settled: the stepped key's success counts as "started at this path".
    let (status, body) = joined(ensure(app), "a settled ensure").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        start_ops(&repo).await,
        starts,
        "a settled ensure starts nothing"
    );
}
