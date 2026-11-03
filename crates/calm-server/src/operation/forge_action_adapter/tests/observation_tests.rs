use super::*;

#[tokio::test]
async fn quiet_forge_action_does_not_claim_until_execution_changes() {
    let fx = forge_runtime_fixture().await;
    let payload = no_event_payload(
        &fx,
        "quiet-forge",
        shell_probe("touch started; until [ -e finish ]; do sleep .05; done"),
        "quiet",
    );
    let id = submit_forge(&fx, payload, "quiet-hash").await.unwrap();
    assert_quiet_execution(&fx, &id).await;
}

#[tokio::test]
async fn quiet_forge_verdict_does_not_claim_for_settled_action() {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        shell_probe("exit 7"),
        ProbeSpec {
            probe_argv: shell_probe("touch started; until [ -e finish ]; do sleep .05; done"),
            output_probe_argv: Some(shell_probe(
                r#"printf '%s\n' '{"headRefOid":"123456","mergeCommit":{"oid":"abcdef"}}'"#,
            )),
        },
    );
    let id = submit_forge(&fx, payload, "quiet-verdict-hash")
        .await
        .unwrap();
    assert_quiet_execution(&fx, &id).await;
}

async fn assert_quiet_execution(fx: &ForgeRuntimeFixture, id: &crate::operation::OperationId) {
    let db = fx.repo.db();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let unclaimed: bool =
                sqlx::query_scalar("SELECT lease_owner IS NULL FROM operations WHERE id=?1")
                    .bind(id)
                    .fetch_one(db.read_pool())
                    .await
                    .unwrap();
            if fx.cwd.path().join("started").exists() && unclaimed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Forge action was released");
    db.writer()
        .execute(sqlx::query("CREATE TABLE forge_claims (op_id TEXT)"))
        .await
        .unwrap();
    db.writer()
        .execute(sqlx::query(
            "CREATE TRIGGER observe_forge_claim AFTER UPDATE OF lease_owner ON operations \
             WHEN NEW.lease_owner IS NOT NULL BEGIN INSERT INTO forge_claims VALUES (NEW.id); END",
        ))
        .await
        .unwrap();
    for _ in 0..3 {
        fx.runtime.sweep_parked().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM forge_claims WHERE op_id=?1")
        .bind(id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    std::fs::write(fx.cwd.path().join("finish"), b"").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(result) = fx.runtime.operation_result(id).await.unwrap() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("execution changes complete Forge without a driving waiter");
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(writes, 0, "quiet Forge execution must not churn claims");
}
