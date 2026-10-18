//! Real WAL contention through production boot, Harness and ACP dispatch.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_settlement_receipt_write_failure_cannot_publish_success() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "settlement").unwrap();
    let (status, body) = stack.post_input(&card, "settle once").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "before-settlement").await;
    let pool = stack.repo().sqlite_pool().unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_acp_settlement BEFORE UPDATE ON acp_submissions ",
        "WHEN NEW.state='completed' BEGIN ",
        "SELECT RAISE(ABORT,'fixture receipt write failure'); END"
    ))
    .execute(&pool)
    .await
    .unwrap();
    std::fs::write(root.path().join("release-settlement"), "").unwrap();
    let outcome = stack.wait_outcomes(&card, 1).await;
    assert_eq!(
        outcome[0]["status"], "failed",
        "undurable success cannot be published"
    );
    let message = outcome[0]["error"]["message"].as_str().unwrap();
    assert!(message.contains("outcome is unknown"));
    assert!(message.contains("will not be resent"));
    assert!(message.contains("start a fresh session"));
    assert!(message.contains("history is kept"));
    assert!(!message.contains("reset"));
    let runtime = stack.runtime(&card).await;
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap()
    );
    sqlx::query("DROP TRIGGER refuse_acp_settlement")
        .execute(&pool)
        .await
        .unwrap();
    stack.shutdown().await;
    let stack = boot(&root).await;
    assert_eq!(stack.outcomes(&card).await[0]["status"], "failed");
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

async fn contention(exhaust: bool) {
    use calm_server::acp_planner::test_seams::{observe_settlement, pause_settlement};
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    let pause = pause_settlement(&runtime.id);
    let mut notes = observe_settlement(&runtime.id);
    let (status, body) = stack.post_input(&card, "retain this exact reply").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_pool().unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let lock = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    pause.release.notify_one();
    // A retry signal proves the real settlement exhausted write admission, including
    // at least one production busy_timeout. Keep the writer until that observed signal.
    let first = tokio::time::timeout(Duration::from_secs(50), notes.recv())
        .await
        .unwrap();
    assert!(stack.outcomes(&card).await.is_empty());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    if exhaust && first == Some("retry") {
        loop {
            let note = tokio::time::timeout(Duration::from_secs(50), notes.recv())
                .await
                .unwrap();
            if note == Some("failed") {
                break;
            }
            assert_eq!(note, Some("retry"));
        }
        assert!(stack.outcomes(&card).await.is_empty());
        assert_eq!(requests(&root, "session/prompt").len(), 1);
    }
    lock.rollback().await.unwrap();
    assert_eq!(first, Some("retry"));
    let outcome = stack.wait_outcomes(&card, 1).await;
    assert_eq!(outcome.len(), 1);
    if exhaust {
        assert_eq!(outcome[0]["status"], "failed");
        assert!(
            outcome[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown")
        );
        assert!(
            calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
                .await
                .unwrap()
        );
        stack.wait_phase(&runtime.id, "turn_completed").await;
        let (status, body) = stack.post_input(&card, "must remain fenced").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let handle = stack.harness(&runtime.id);
        tokio::time::timeout(Duration::from_secs(20), async {
            while handle.refused_issuances_for_test() == 0 {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("next issuance observes the durable fence");
        assert!(
            handle
                .issuance_block()
                .await
                .is_some_and(|reason| reason.contains("Previous ACP outcome is unknown"))
        );
        assert_eq!(requests(&root, "session/prompt").len(), 1);
    } else {
        assert_eq!(outcome[0]["status"], "completed");
        // The adapter records the outcome before the harness consumes its final items.
        stack.wait_phase(&runtime.id, "turn_completed").await;
        let (state, json): (String, String) = sqlx::query_as(
            "SELECT state,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
        )
        .bind(&runtime.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "completed");
        let mut durable: Value = serde_json::from_str(&json).unwrap();
        let items = durable.as_object_mut().unwrap().remove("items").unwrap();
        assert_eq!(durable, outcome[0]);
        let completed: Vec<Value> = items
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["method"] == "item/completed")
            .map(|item| item["params"].clone())
            .collect();
        // The harness also persists its authoritative user-input projection, which
        // is not a provider final item and does not belong to the receipt's item list.
        let published: Vec<_> =
            claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed")
                .await
                .into_iter()
                .filter(|frame| frame["_projection"] != true)
                .collect();
        let prompts = requests(&root, "session/prompt");
        let prompt = prompts[0]["params"]["prompt"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["text"]
            .as_str()
            .unwrap();
        let expected = format!("reply: {prompt}");
        assert!(
            completed
                .iter()
                .any(|frame| frame["item"]["text"] == expected),
            "the original native reply is retained: {completed:?}"
        );
        assert_eq!(
            completed, published,
            "the original final items are durable and published once"
        );
        assert_eq!(requests(&root, "session/prompt").len(), 1);
        assert_eq!(
            turn(&stack, &card, "next accepted", 2).await["status"],
            "completed"
        );
        assert_eq!(requests(&root, "session/prompt").len(), 2);
    }
    stack.shutdown().await;
    if exhaust {
        let stack = boot(&root).await;
        let state: String =
            sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
                .bind(&runtime.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            state, "unknown",
            "restart retires the unresolved receipt without replay"
        );
        assert_eq!(stack.outcomes(&card).await.len(), 1);
        assert_eq!(requests(&root, "session/prompt").len(), 1);
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_settlement_known_outcome_survives_writer_contention_without_resend() {
    contention(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_settlement_budget_exhaustion_keeps_unknown_fence_without_resend() {
    contention(true).await;
}
