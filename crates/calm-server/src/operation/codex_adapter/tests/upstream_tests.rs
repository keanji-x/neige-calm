//! #1777 — a codex worker's lease starts from the attached repository's
//! upstream, fetched on the submit path and read locally in `prepare_tx`.

use super::*;
use crate::operation::workspace_lease::upstream_tests::{
    attach_origin, break_origin, git, kernel_ref, user_ref_state,
};

async fn lease_base_source(harness: &WorkerLeaseHarness, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT base_source FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap()
}

/// The repro: the attached checkout sits on a branch whose upstream has moved
/// on (fetched, never merged — the production checkout's shape). The lease
/// starts from the upstream, not from the lagging HEAD, and says so.
#[tokio::test]
async fn lease_in_repo_lagging_its_upstream_starts_from_upstream() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let tip = origin.commit("landed upstream");
    git(attached, &["fetch", "-q", "origin"]);
    let head = git(attached, &["rev-parse", "HEAD"]);
    assert_ne!(head, tip, "HEAD lags its upstream");

    let (output, _) = prepare_worker(&harness, "lagging").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), tip);
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(lease_base_source(&harness, &lease_id).await, "upstream");
}

/// Without an upstream the base is HEAD, recorded as `head`.
#[tokio::test]
async fn lease_without_upstream_keeps_head() {
    let harness = worker_lease_harness().await;
    let head = git(harness.repo_root.path(), &["rev-parse", "HEAD"]);
    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "no-upstream"))
        .await;

    let (output, _) = prepare_worker(&harness, "no-upstream").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), head);
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(lease_base_source(&harness, &lease_id).await, "head");
}

/// `before_insert` (the submit path) fetches the upstream the checkout has
/// never fetched; `prepare_tx` then starts the lease from it. The user's refs
/// and `FETCH_HEAD` are as they were.
#[tokio::test]
async fn before_insert_fetches_the_upstream_prepare_reads() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let tip = origin.commit("landed upstream, not fetched by the user");
    let before = user_ref_state(attached);

    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "fresh"))
        .await;
    let (output, _) = prepare_worker(&harness, "fresh").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), tip);
    assert_eq!(git(attached, &["rev-parse", &kernel_ref(&origin)]), tip);
    assert_eq!(user_ref_state(attached), before);
}

/// `prepare_tx` never touches the network: the upstream moves after the last
/// submit-path fetch, and the lease starts from what that fetch left in the
/// kernel ref, which `prepare_tx` does not move.
#[tokio::test]
async fn prepare_tx_never_fetches() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let fetched = origin.commit("fetched on submit");
    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "first"))
        .await;
    let moved = origin.commit("landed after the fetch");

    let (output, _) = prepare_worker(&harness, "no-fetch").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), fetched);
    assert_ne!(fetched, moved);
    assert_eq!(git(attached, &["rev-parse", &kernel_ref(&origin)]), fetched);
}

/// The submit path's fetch fails: the lease starts from the last known
/// upstream and is still recorded as `upstream`.
#[tokio::test]
async fn failed_fetch_leases_from_the_last_known_upstream() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let tracking = git(
        attached,
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{}", origin.branch),
        ],
    );
    origin.commit("unreachable now");
    break_origin(attached);

    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "offline"))
        .await;
    let (output, _) = prepare_worker(&harness, "offline").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), tracking);
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(lease_base_source(&harness, &lease_id).await, "upstream");
}
