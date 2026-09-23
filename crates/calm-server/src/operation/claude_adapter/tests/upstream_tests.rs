//! #1777 — a claude worker's lease starts from the attached repository's
//! upstream, fetched on the submit path and read locally in `prepare_tx`.

use super::*;
use crate::operation::workspace_lease::upstream_tests::{
    attach_origin, git, kernel_ref, transport_count, witness_transport,
};

/// `before_insert` fetches the upstream; `prepare_tx` starts the lease from
/// it and records `upstream`.
#[tokio::test]
async fn claude_lease_starts_from_the_fetched_upstream() {
    let harness = claude_worker_harness().await;
    let attached = harness.workspace.path();
    let origin = attach_origin(attached);
    let tip = origin.commit("landed upstream");
    assert_ne!(git(attached, &["rev-parse", "HEAD"]), tip);

    harness
        .adapter
        .before_insert(&claude_worker_payload(&harness.track_id, "fresh"))
        .await;
    let (output, _, _) = prepare_claude_worker(&harness, "fresh").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), tip);
    let lease_id = output.output_string("lease_id", "test").unwrap();
    let source: String =
        sqlx::query_scalar("SELECT base_source FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(source, "upstream");
}

/// `prepare_tx` never touches the network: a transport witness on the remote
/// counts at least one fetch during `before_insert` and none during
/// `prepare_tx`, and the lease starts from the kernel ref the submit-path
/// fetch left.
#[tokio::test]
async fn claude_prepare_tx_never_fetches() {
    let harness = claude_worker_harness().await;
    let attached = harness.workspace.path();
    let origin = attach_origin(attached);
    let witness = witness_transport(attached);
    let fetched = origin.commit("fetched on submit");
    harness
        .adapter
        .before_insert(&claude_worker_payload(&harness.track_id, "no-fetch"))
        .await;
    let submitted = transport_count(&witness);
    assert!(submitted >= 1, "the submit path reached the remote");
    let moved = origin.commit("landed after the fetch");

    let (output, _, _) = prepare_claude_worker(&harness, "no-fetch").await;

    assert_eq!(
        transport_count(&witness),
        submitted,
        "prepare_tx reached the remote"
    );
    assert_eq!(output.output_string("base_sha", "test").unwrap(), fetched);
    assert_ne!(fetched, moved);
    assert_eq!(git(attached, &["rev-parse", &kernel_ref(&origin)]), fetched);
}
