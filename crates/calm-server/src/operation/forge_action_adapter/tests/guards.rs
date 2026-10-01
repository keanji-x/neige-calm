//! Real execution admission, cancellation, failure and recovery ownership.
use super::*;

#[tokio::test]
async fn forge_writer_reference_blocks_readers_until_cancelled_execution_stops() {
    let fx = forge_runtime_fixture().await;
    let started = fx.cwd.path().join("started");
    let payload = merge_payload(
        &fx,
        shell_probe(&format!(
            "touch {}; sleep 60",
            shell_quote(started.to_str().unwrap())
        )),
        ProbeSpec {
            probe_argv: shell_probe("exit 1"),
            output_probe_argv: Some(shell_probe("exit 0")),
        },
    );
    let op = submit_forge(&fx, payload, "guard-cancel").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let readers_blocked = !crate::db::sqlite::track_available(
        &mut tx,
        &fx.track_id,
        "",
        calm_types::workspace_access::WorkspaceAccess::ReadOnly,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let held:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='forge' AND holder_id=?1 AND state='held'").bind(&op).fetch_one(fx.repo.pool()).await.unwrap();
    // Stop the real spawned execution even when a regression assertion would fail.
    assert!(
        fx.runtime
            .cancel_parked(&op, "test cancellation")
            .await
            .unwrap()
    );
    let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='forge' AND state IN ('held','releasing')").fetch_one(fx.repo.pool()).await.unwrap();
    assert!(
        readers_blocked,
        "Forge execution must continue blocking readers after the caller turn"
    );
    assert_eq!(held, 1, "prepare must acquire the durable action reference");
    assert_eq!(
        remaining, 0,
        "cancel must confirm process stop before releasing references"
    );
}

#[tokio::test]
async fn forge_spawn_failure_releases_prepared_writer_reference() {
    let fx = forge_runtime_fixture().await;
    let mut payload = merge_payload(
        &fx,
        shell_probe("exit 0"),
        ProbeSpec {
            probe_argv: shell_probe("exit 1"),
            output_probe_argv: Some(shell_probe("exit 0")),
        },
    );
    // A directory blocks writing the expected durable result artifact after preparation.
    payload.result_path = fx.cwd.path().join("file-parent/result");
    std::fs::write(fx.cwd.path().join("file-parent"), "occupied").unwrap();
    let result = submit_and_wait(&fx, payload, "spawn-failure-guard").await;
    assert!(matches!(result.outcome, OperationOutcome::Failed { .. }));
    let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='forge' AND state IN ('held','releasing')").fetch_one(fx.repo.pool()).await.unwrap();
    assert_eq!(
        remaining, 0,
        "pre-spawn failure must compensate the durable write reference"
    );
}

#[tokio::test]
async fn forge_stale_owner_cannot_issue_probe_or_complete_operation() {
    let (fx, frozen, claimed) = claimed_forge_fixture("owner-fence").await;
    let op_id = claimed.id.clone();
    let current_owner = claimed.lease_owner.as_deref().unwrap();
    let sentinel = fx.cwd.path().join("stale-probe-issued");
    let argv = shell_probe(&format!(
        "touch {}",
        shell_quote(sentinel.to_str().unwrap())
    ));
    let rejected = lifecycle::probe(
        fx.repo.pool(),
        &op_id,
        "stale-owner",
        &frozen,
        &argv,
        fx.repo.as_ref(),
    )
    .await;
    assert!(rejected.is_err());
    assert!(
        !sentinel.exists(),
        "a stale callback must never issue its probe process"
    );
    assert!(
        complete_forge_op_failed(
            fx.repo.pool(),
            &OperationCompletionBus::new(),
            &op_id,
            "stale-owner",
            "stale outcome".into(),
            None
        )
        .await
        .is_err()
    );
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
        .bind(&op_id)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        phase, "parked",
        "stale completion must preserve the current owner's operation"
    );
    sqlx::query("UPDATE operations SET lease_until_ms=?2 WHERE id=?1")
        .bind(&op_id)
        .bind(now_ms() - 1)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    assert!(
        lifecycle::probe(
            fx.repo.pool(),
            &op_id,
            current_owner,
            &frozen,
            &argv,
            fx.repo.as_ref()
        )
        .await
        .is_err(),
        "an expired owner cannot issue a new probe either"
    );
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE state='held' AND holder_kind='forge'",
    )
    .fetch_one(fx.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        held, 1,
        "rejected issuance must create no child and retain the parent for recovery"
    );
    lifecycle::finish(fx.repo.pool(), &op_id).await.unwrap();
}

#[tokio::test]
async fn forge_long_probe_keeps_operation_owner_and_writer_reference() {
    let fx = forge_runtime_fixture().await;
    let started = fx.cwd.path().join("probe-started");
    let payload = merge_payload(
        &fx,
        shell_probe("exit 7"),
        ProbeSpec {
            probe_argv: shell_probe(&format!(
                "touch {}; sleep 24; exit 0",
                shell_quote(started.to_str().unwrap())
            )),
            output_probe_argv: Some(shell_probe(
                "printf '%s\n' '{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{\"oid\":\"2222222222222222222222222222222222222222\"}}'",
            )),
        },
    );
    let op = submit_forge(&fx, payload, "long-probe-owner")
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (owner, before): (String, i64) =
        sqlx::query_as("SELECT lease_owner,lease_until_ms FROM operations WHERE id=?1")
            .bind(&op)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(21)).await;
    let (current, after): (String, i64) =
        sqlx::query_as("SELECT lease_owner,lease_until_ms FROM operations WHERE id=?1")
            .bind(&op)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE holder_kind='forge' AND state='held'",
    )
    .fetch_one(fx.repo.pool())
    .await
    .unwrap();
    let result = fx.runtime.wait(&op).await.unwrap();
    assert_eq!(
        current, owner,
        "a long probe must retain its exact operation owner"
    );
    assert!(
        after >= before + 15_000,
        "owned recovery must renew while the probe is running"
    );
    assert_eq!(
        held, 2,
        "both the stopped action and its running probe retain durable references"
    );
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='forge' AND state IN ('held','releasing')").fetch_one(fx.repo.pool()).await.unwrap();
    assert_eq!(
        remaining, 0,
        "all action and probe references release only after actual stop"
    );
}

async fn claimed_forge_fixture(
    payload_hash: &str,
) -> (ForgeRuntimeFixture, FrozenForge, Operation) {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        shell_probe("exit 0"),
        ProbeSpec {
            probe_argv: shell_probe("exit 0"),
            output_probe_argv: Some(shell_probe("exit 0")),
        },
    );
    let repo = SqlxOperationRepo::new(fx.repo.pool().clone());
    let op_id = repo
        .insert_operation(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(payload.idem_key.clone()),
                payload_hash: payload_hash.into(),
            },
            serde_json::to_value(&payload).unwrap(),
        )
        .await
        .unwrap();
    let op = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    let (prepared, _) = repo
        .prepare_tx_and_advance(&op, &ForgeActionAdapter::new())
        .await
        .unwrap()
        .unwrap();
    let output = prepared.tx_output.as_ref().expect("prepared output");
    let op = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    repo.set_phase(&op, crate::operation::Phase::SpawnStarted)
        .await
        .unwrap()
        .unwrap();
    let op = repo.claim_drive_batch(1).await.unwrap().pop().unwrap();
    let artifacts = SpawnArtifacts {
        pid: 2,
        pgid: 2,
        start_time: 0,
        boot_id: "prior-test-boot".into(),
        log_path: None,
        extra: json!({}),
    };
    repo.record_spawn_artifacts(&op, &artifacts).await.unwrap();
    repo.set_parked(&op, now_ms() + 60_000)
        .await
        .unwrap()
        .unwrap();
    let claimed = repo.claim_parked(&op_id).await.unwrap().unwrap();
    let frozen = FrozenForge::from_output(&output).unwrap();
    (fx, frozen, claimed)
}

#[tokio::test]
async fn forge_delayed_probe_issuer_cannot_write_after_recovery_releases_references() {
    let (fx, frozen, claimed) = claimed_forge_fixture("delayed-issuer").await;
    let op = claimed.id.clone();
    let pause = lifecycle::ProbeIssuancePause {
        op: op.clone(),
        prepared: Arc::new(tokio::sync::Notify::new()),
        resume_prepare: Arc::new(tokio::sync::Notify::new()),
        spawned: Arc::new(tokio::sync::Notify::new()),
        resume_spawn: Arc::new(tokio::sync::Notify::new()),
    };
    *lifecycle::PROBE_ISSUANCE_PAUSE.lock().unwrap() = Some(pause.clone());
    let sentinel = fx.cwd.path().join("late-writer");
    let argv = shell_probe(&format!(
        "touch {}; sleep 1",
        shell_quote(sentinel.to_str().unwrap())
    ));
    let pool = fx.repo.pool().clone();
    let repo = fx.repo.clone();
    let task_op = op.clone();
    let task = tokio::spawn(async move {
        lifecycle::probe(
            &pool,
            &task_op,
            claimed.lease_owner.as_deref().unwrap(),
            &frozen,
            &argv,
            repo.as_ref(),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.prepared.notified())
        .await
        .unwrap();
    // Recovery sees no process yet and closes the independent child and parent references.
    lifecycle::finish_children(fx.repo.pool(), &op)
        .await
        .unwrap();
    lifecycle::finish(fx.repo.pool(), &op).await.unwrap();
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let readers_available = crate::db::sqlite::track_available(
        &mut tx,
        &fx.track_id,
        "",
        calm_types::workspace_access::WorkspaceAccess::ReadOnly,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    pause.resume_prepare.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.spawned.notified())
        .await
        .unwrap();
    // An unguarded provider would now run, while the handshake wrapper must still wait.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let wrote = sentinel.exists();
    pause.resume_spawn.notify_one();
    let result = task.await.unwrap();
    *lifecycle::PROBE_ISSUANCE_PAUSE.lock().unwrap() = None;
    assert!(readers_available);
    assert!(
        !wrote && !sentinel.exists(),
        "a delayed issuer must never write after its reference was released"
    );
    assert!(
        result.is_err(),
        "recovery's closed child reference must reject recording and go-token release"
    );
}
