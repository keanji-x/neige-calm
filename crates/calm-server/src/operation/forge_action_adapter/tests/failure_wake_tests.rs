use super::*;

#[tokio::test]
async fn forge_failure_before_engine_ownership_after_pending_has_durable_wake() {
    let fx = forge_runtime_fixture().await;
    let guard =
        crate::per_card_lock::lock_key(fx.runtime.track_locks_for_test(), &fx.track_id).await;
    let mut payload = merge_payload(
        &fx,
        shell_probe("exit 7"),
        ProbeSpec {
            probe_argv: shell_probe("exit 1"),
            output_probe_argv: Some(shell_probe("printf '{}'")),
        },
    );
    let blocker = fx.results.path().join("regular-file");
    std::fs::write(&blocker, "not a directory").unwrap();
    payload.result_path = blocker.join("result.json");
    let id = submit_forge(&fx, payload, "deferred-spawn-failure")
        .await
        .unwrap();
    assert!(fx.runtime.operation_result(&id).await.unwrap().is_none());
    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(result) = fx.runtime.operation_result(&id).await.unwrap() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("deferred admission must progress without a driving wait");
    assert!(matches!(
        result.outcome,
        OperationOutcome::Failed { .. } | OperationOutcome::Stuck { .. }
    ));
    let wakes = event_payloads(&fx.repo, "track.wake_requested").await;
    assert_eq!(
        wakes.len(),
        1,
        "pending was already returned, so pre-ownership failure still owes a result"
    );
    assert_eq!(wakes[0]["key"], id);
}

async fn settled(fx: &ForgeRuntimeFixture, id: &str) -> OperationResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(result) = fx.runtime.operation_result(&id.to_string()).await.unwrap() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("observer settles without a driving wait")
}

#[tokio::test]
async fn failed_forge_wake_and_result_commit_atomically() {
    let fx = forge_runtime_fixture().await;
    sqlx::query(
        "CREATE TRIGGER reject_failure_wake BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' BEGIN \
         SELECT RAISE(ABORT, 'injected failure wake rejection'); END",
    )
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let payload = merge_payload(
        &fx,
        shell_probe("exit 7"),
        ProbeSpec {
            probe_argv: shell_probe("exit 1"),
            output_probe_argv: Some(shell_probe("printf '{}'")),
        },
    );
    let id = submit_forge(&fx, payload, "failure-atomicity")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let error: Option<String> =
                sqlx::query_scalar("SELECT last_error FROM operations WHERE id=?1")
                    .bind(&id)
                    .fetch_one(fx.repo.pool())
                    .await
                    .unwrap();
            if error.is_some_and(|error| error.contains("injected failure wake rejection")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        fx.runtime.operation_result(&id).await.unwrap().is_none(),
        "a missing wake must roll back terminal settlement"
    );
    assert_eq!(event_count(&fx.repo, "track.wake_requested").await, 0);
    sqlx::query("DROP TRIGGER reject_failure_wake")
        .execute(fx.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
        .bind(&id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.runtime.sweep_parked().await.unwrap();
    assert!(matches!(
        settled(&fx, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    fx.runtime.sweep_parked().await.unwrap();
    assert_eq!(event_count(&fx.repo, "track.wake_requested").await, 1);
}

#[tokio::test]
async fn unknown_forge_probe_reports_unknown_and_quiet_actions_stay_quiet() {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        shell_probe("exit 7"),
        ProbeSpec {
            probe_argv: shell_probe("exit 2"),
            output_probe_argv: Some(shell_probe("printf '{}'")),
        },
    );
    let id = submit_forge(&fx, payload, "unknown-failure").await.unwrap();
    assert!(matches!(
        settled(&fx, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    let wakes = event_payloads(&fx.repo, "track.wake_requested").await;
    assert_eq!(wakes.len(), 1);
    assert!(
        wakes[0]["text"]
            .as_str()
            .unwrap()
            .contains("verdict unknown")
    );
    assert!(!wakes[0]["text"].as_str().unwrap().contains("not landed"));
    let payload = no_event_payload(&fx, "quiet-failure", shell_probe("exit 9"), "quiet-failure");
    let quiet = submit_forge(&fx, payload, "quiet-failure").await.unwrap();
    assert!(matches!(
        settled(&fx, &quiet).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    assert_eq!(event_count(&fx.repo, "track.wake_requested").await, 1);
}
