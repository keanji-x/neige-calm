use super::*;

#[tokio::test]
async fn c2b_gate_run_waits_without_idle_claim_writes() {
    let fx = world(Duration::from_millis(100)).await;
    let finish = outside(&fx, "quiet-gate-finish");
    let a = running(
        &fx,
        "quiet-gate",
        json!([{"name":"wait","cmd":wait_for(&finish)}]),
    )
    .await;
    gate_run(&fx, &a, Some("test: quiet gate")).await.unwrap();
    wait_until(async || {
        let current = op(&fx, &a.task.id).await;
        record(&current)["release"].get("Released").is_some() && current.lease_owner.is_none()
    })
    .await;
    let owner = op(&fx, &a.task.id).await.id;
    fx.db()
        .writer()
        .execute(sqlx::query("CREATE TABLE idle_claims (op_id TEXT)"))
        .await
        .unwrap();
    fx.db()
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER observe_idle_claims AFTER UPDATE OF lease_owner ON operations \
            WHEN NEW.lease_owner IS NOT NULL BEGIN INSERT INTO idle_claims VALUES (NEW.id); END",
        ))
        .await
        .unwrap();
    for _ in 0..3 {
        fx.execution.lifecycle.drive([serde_json::from_value(
            record(&op(&fx, &a.task.id).await)["execution_id"].clone(),
        )
        .unwrap()]);
        fx.runtime.sweep_parked().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM idle_claims WHERE op_id=?")
        .bind(&owner)
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    touch(&finish);
    assert!(run_result(&fx, &a.task.id, 1).await.verdict.passed);
    assert_eq!(
        writes, 0,
        "stable execution and recovery sweeps must not churn claims"
    );
}
