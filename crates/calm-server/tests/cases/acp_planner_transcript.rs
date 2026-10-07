//! Receipt replay repairs missing prefixes without changing logical item order.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_receipt_recovery_restores_native_item_order() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "ordered recovery", 1).await;
    let runtime = stack.runtime(&card).await;
    let before = stack
        .repo()
        .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_pool().unwrap();
    let mut snapshot = stack.harness(&runtime.id).snapshot().await;
    stack.shutdown().await;
    let receipt: String =
        sqlx::query_scalar("SELECT outcome_json FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&runtime.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    let first = receipt["items"][0]["params"]["item"]["id"]
        .as_str()
        .unwrap();
    snapshot.phase = calm_server::harness::HarnessPhaseTag::TurnRunning;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM harness_items WHERE card_id=?1 AND item_uuid=?2")
        .bind(&card)
        .bind(first)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&snapshot).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let stack = boot(&root).await;
    let rows = stack
        .repo()
        .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
        .await
        .unwrap();
    let ids: Vec<&str> = receipt["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| frame["params"]["item"]["id"].as_str().unwrap())
        .collect();
    let positions: Vec<usize> = ids
        .iter()
        .map(|id| {
            rows.iter()
                .position(|row| row.item_uuid.as_deref() == Some(*id))
                .unwrap()
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "native items must keep their receipt order: {positions:?}"
    );
    for row in before
        .iter()
        .filter(|row| !ids.contains(&row.item_uuid.as_deref().unwrap_or("")))
    {
        assert!(
            rows.contains(row),
            "receipt repair must retain unrelated input/history"
        );
    }
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
    let stack = boot(&root).await;
    assert_eq!(
        stack
            .repo()
            .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
            .await
            .unwrap(),
        rows,
        "repeated recovery must be idempotent in row identity and order"
    );
    stack.shutdown().await;
}
