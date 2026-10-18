//! A prompt the agent answers with an error is a known failure; a lost answer stays unknown.
use super::*;

const AGENT_ERROR: &str = "Internal error: Insufficient balance; top up the account";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_prompt_error_fails_the_turn_with_its_text_and_the_next_input_runs() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "prompt-error").unwrap();
    let outcome = turn(&stack, &card, "model refuses", 1).await;
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert_eq!(outcome["error"]["message"], AGENT_ERROR, "{outcome}");
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    assert!(
        !calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap(),
        "an answered prompt settles its receipt"
    );
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter().any(|row| row["item"]["text"] == "partial"),
        "text streamed before the error is kept: {rows:?}"
    );

    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let outcome = turn(&stack, &card, "after the error", 2).await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    let handle = stack.harness(&runtime.id);
    assert_eq!(handle.refused_issuances_for_test(), 0);
    assert_eq!(handle.issuance_block().await, None);
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_recovery_acp_prompt_connection_loss_stays_unknown_and_blocks_the_next_input() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "lost").unwrap();
    let outcome = turn(&stack, &card, "perform once", 1).await;
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown"),
        "{outcome}"
    );

    let (status, run) = stack
        .send("GET", &format!("/api/cards/{card}/planner/run"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["phase"], "turn_completed", "{run}");
    assert_eq!(run["running_turn"], serde_json::Value::Null);
    assert_eq!(run["pending"], serde_json::json!([]));
    assert_eq!(
        run["recovery"],
        serde_json::json!({
            "cause": "unknown_outcome", "action": "restart_session"
        }),
        "an empty queue still has an unresolved receipt: {run}"
    );

    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let (status, body) = stack.post_input(&card, "later input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    let handle = stack.harness(&runtime.id);
    tokio::time::timeout(Duration::from_secs(20), async {
        while handle.refused_issuances_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the production issuance path must actually hit the durable fence");
    assert!(
        handle.issuance_block().await.is_some_and(|reason| {
            reason.contains("Previous ACP outcome is unknown")
                && reason.contains("Start a fresh session")
                && reason.contains("nothing will be resent")
                && !reason.contains("Reset")
        }),
        "the card is blocked until a fresh session starts"
    );
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "nothing is resent"
    );
    stack.shutdown().await;
}
