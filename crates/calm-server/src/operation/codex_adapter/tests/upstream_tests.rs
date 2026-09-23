//! #1777 — a codex worker's lease starts from the attached repository's
//! upstream when HEAD is at or behind it, from HEAD when HEAD is ahead, and is
//! refused when the two diverged; fetched on the submit path, read locally in
//! `prepare_tx`.

use super::*;
use crate::operation::workspace_lease::upstream_tests::{
    attach_origin, break_origin, commit_locally, git, kernel_ref, transport_count, user_ref_state,
    witness_transport,
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

/// HEAD ahead of its upstream (the human's unpushed commit): the lease starts
/// from HEAD, recorded as `head`, and the worker's provisioned worktree has
/// the human's commit in it.
#[tokio::test]
async fn lease_ahead_of_upstream_starts_from_head_with_the_unpushed_commit() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let _origin = attach_origin(attached);
    let head = commit_locally(attached, "unpushed.txt");
    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "ahead"))
        .await;

    let (mut output, _, op) = prepare_worker_and_op(&harness, "ahead", "ahead").await;

    assert_eq!(output.output_string("base_sha", "test").unwrap(), head);
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(lease_base_source(&harness, &lease_id).await, "head");

    let card_id = output.output_string("card_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();
    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let kind = harness
        .adapter
        .app_server_interact_kind(&output, &op)
        .unwrap();
    op_repo
        .set_phase(&op, Phase::AppServerInteract { kind })
        .await
        .unwrap()
        .expect("the claimed op moves to app_server_interact");
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|claimed| claimed.id == op.id)
        .expect("the op is re-claimed in app_server_interact");
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let ctx = SpawnCtx::new(
        route_repo,
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    provision_codex_worker_workspace(
        &ctx,
        &harness.adapter.card_role_cache,
        &harness.adapter.track_area_cache,
        &op,
        &mut output,
    )
    .await
    .expect("spawn provisions the prepared lease");

    assert_eq!(git(Path::new(&cwd), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read_to_string(Path::new(&cwd).join("unpushed.txt")).unwrap(),
        "unpushed\n",
        "the human's unpushed commit is in the worker's worktree"
    );
    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
}

/// HEAD and its upstream diverged: `prepare_tx` refuses with the
/// client-class `Conflict` carrying the machine code, and no lease row exists.
#[tokio::test]
async fn diverged_checkout_refuses_the_lease() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let head = commit_locally(attached, "unpushed.txt");
    let upstream = origin.commit("landed upstream");
    git(attached, &["fetch", "-q", "origin"]);

    let payload = worker_payload(&harness.track_id, "diverged");
    sqlx::query(
        "INSERT INTO tasks \
         (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'diverged', 'codex', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(format!("{}:diverged", harness.track_id))
    .bind(&harness.track_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let err = harness
        .adapter
        .prepare_tx(
            &mut tx,
            &payload,
            &worker_op("op-diverged", payload.clone()),
        )
        .await
        .expect_err("a diverged checkout is refused");
    tx.rollback().await.unwrap();

    let CalmError::Conflict(text) = &err else {
        panic!("the refusal is a Conflict: {err:?}");
    };
    assert!(
        text.starts_with("refused: attached-repo-diverged:"),
        "{text}"
    );
    assert!(text.contains(&head) && text.contains(&upstream), "{text}");
    assert!(text.contains("1 ahead, 1 behind"), "{text}");
    let leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(leases, 0);
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

/// `prepare_tx` never touches the network. A transport witness on the remote
/// counts every fetch that reaches it — the kernel's or any other: at least
/// one during `before_insert`, none during `prepare_tx`. The upstream moves
/// after the submit-path fetch, and the lease starts from what that fetch
/// left in the kernel ref.
#[tokio::test]
async fn prepare_tx_never_fetches() {
    let harness = worker_lease_harness().await;
    let attached = harness.repo_root.path();
    let origin = attach_origin(attached);
    let witness = witness_transport(attached);
    let fetched = origin.commit("fetched on submit");
    harness
        .adapter
        .before_insert(&worker_payload(&harness.track_id, "no-fetch"))
        .await;
    let submitted = transport_count(&witness);
    assert!(submitted >= 1, "the submit path reached the remote");
    let moved = origin.commit("landed after the fetch");

    let (output, _) = prepare_worker(&harness, "no-fetch").await;

    assert_eq!(
        transport_count(&witness),
        submitted,
        "prepare_tx reached the remote"
    );
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
    let tracking = git(attached, &["rev-parse", &origin.tracking_ref()]);
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

/// Workers dispatched in parallel: N concurrent `before_insert` calls for one
/// upstream run ONE fetch and share its outcome — no lost ref lock recorded as
/// a failure, so the kernel's receipt stays authoritative for the lease.
/// Deterministic: the remote's upload-pack blocks on a release file; the test
/// releases it only once the witness shows the one transport call AND all
/// eight callers are registered on the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submits_share_one_fetch() {
    use crate::operation::workspace_lease::upstream_fetch::FetchProvenance;
    let harness = Arc::new(worker_lease_harness().await);
    let attached = harness.repo_root.path().to_path_buf();
    let origin = attach_origin(&attached);
    let gate = tempfile::tempdir().unwrap();
    let release = gate.path().join("release");
    let witness = crate::operation::workspace_lease::upstream_tests::witness_transport_then(
        &attached,
        &format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done; git-upload-pack",
            release.display()
        ),
    );
    let tip = origin.commit("landed upstream");
    let key = (
        crate::operation::workspace_lease::base::lease_git_common_dir(&attached).unwrap(),
        kernel_ref(&origin),
    );

    let callers: Vec<_> = (0..8)
        .map(|n| {
            let harness = harness.clone();
            let payload = worker_payload(&harness.track_id, &format!("parallel-{n}"));
            tokio::spawn(async move { harness.adapter.before_insert(&payload).await })
        })
        .collect();
    let registered = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if transport_count(&witness) == 1
                && FetchProvenance::global().registered_callers(&key) == 8
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    std::fs::write(&release, "").unwrap();
    registered.expect("one transport call and eight registered callers");
    for caller in callers {
        caller.await.unwrap();
    }

    assert_eq!(transport_count(&witness), 1, "one fetch for eight submits");
    assert!(
        FetchProvenance::global().last_fetch_succeeded(&key),
        "no false failure was recorded"
    );
    let (output, _) = prepare_worker(&harness, "parallel-0").await;
    assert_eq!(output.output_string("base_sha", "test").unwrap(), tip);
}
