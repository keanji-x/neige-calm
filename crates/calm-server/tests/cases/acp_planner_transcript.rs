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
    let pool = stack.repo().sqlite_db().unwrap();
    let mut snapshot = stack.harness(&runtime.id).snapshot().await.unwrap();
    stack.shutdown().await;
    let receipt: String =
        sqlx::query_scalar("SELECT outcome_json FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
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
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&snapshot).unwrap())
        .execute(&mut **tx)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_usage_update_reaches_planner_run_and_survives_restart() {
    check_usage(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_context_usage_reaches_planner_run_and_survives_restart() {
    check_usage(true).await;
}

async fn boot_usage(root: &Root, grok: bool) -> Stack {
    if grok {
        super::acp_grok_provider::boot_grok(root).await
    } else {
        boot(root).await
    }
}
async fn check_usage(grok: bool) {
    let root = Root::new("unused");
    let stack = boot_usage(&root, grok).await;
    let (_, card) = if grok {
        super::acp_grok_provider::create_grok(&stack).await
    } else {
        create(&stack).await
    };
    std::fs::write(root.path().join("scenario"), "usage").unwrap();
    assert_eq!(
        turn(&stack, &card, "report usage", 1).await["status"],
        "completed"
    );
    let path = format!("/api/cards/{card}/planner/run");
    let (status, run) = stack.send("GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let usage = run["token_usage"].clone();
    assert_eq!(
        usage["used_tokens"], 24_100,
        "latest occupancy wins, including decreases"
    );
    assert_eq!(usage["context_window"], 200_000);
    let expected = (24_100.0 - 12_000.0) / (200_000.0 - 12_000.0) * 100.0;
    assert!((usage["percent"].as_f64().unwrap() - expected).abs() < 1e-9);
    assert!(usage["at_ms"].as_i64().unwrap() > 0);
    stack.shutdown().await;
    let stack = boot_usage(&root, grok).await;
    let (status, restored) = stack.send("GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["token_usage"], usage);
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}
