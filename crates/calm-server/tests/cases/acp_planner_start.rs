//! Receipt admission through the real owner-bound managed start.
use super::*;
fn launches(root: &Root) -> usize {
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
        .count()
}
async fn receipt(stack: &Stack, worker: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT client_id,turn_id,input_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_pipe_failure_after_prepare_retires_original_batch_without_resend() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    std::fs::write(root.path().join("scenario"), "closed-input").unwrap();
    let outcome = turn(&stack, &card, "original batch", 1).await;
    let (_, turn_id, input) = receipt(&stack, &runtime.id).await;
    assert_eq!(outcome["id"], turn_id);
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown")
    );
    let input: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(input["queue"].as_array().unwrap().len(), 1);
    assert!(
        input["input"][0]["text"]
            .as_str()
            .unwrap()
            .contains("original batch")
    );
    assert!(
        stack
            .harness(&runtime.id)
            .snapshot()
            .await
            .unwrap()
            .pending_entries()
            .is_empty()
    );
    assert!(
        requests(&root, "session/prompt").is_empty(),
        "broken pipe must precede any peer read"
    );
    let before = launches(&root);
    let (status, body) = stack.post_input(&card, "later batch").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handle = stack.harness(&runtime.id);
    tokio::time::timeout(Duration::from_secs(20), async {
        while handle.refused_issuances_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("later input must reach the durable fence");
    assert_eq!(launches(&root), before);
    assert!(requests(&root, "session/prompt").is_empty());
    let pending = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .unwrap()
        .pending_entries();
    assert_eq!(pending.len(), 1);
    assert!(format!("{:?}", pending[0]).contains("later batch"));
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_two_owner_decoy_is_untouched_by_bound_start_interrupt_and_shutdown() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, a) = create(&stack).await;
    let (_, b) = create(&stack).await;
    let ra = stack.runtime(&a).await;
    let rb = stack.runtime(&b).await;
    // Restore the same projection key on both idle owners, then let the real
    // input route recover each harness and reconcile its queued human input.
    for runtime in [&ra, &rb] {
        let old = stack.state.harness.remove(&runtime.id).unwrap();
        old.shutdown().await.unwrap();
        let mut snapshot = old.snapshot().await.unwrap();
        snapshot.projection_client_id = Some(calm_server::harness::queue::QueueEntryId::from_wire(
            "shared-key".into(),
        ));
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
            .bind(&runtime.id)
            .bind(serde_json::to_string(&snapshot).unwrap())
            .execute(stack.repo().sqlite_db().unwrap().writer_pool())
            .await
            .unwrap();
    }
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&b, "owner input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.wait_phase(&rb.id, "turn_running").await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while requests(&root, "session/prompt").is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let hb = stack.harness(&rb.id);
    let before = serde_json::to_value(hb.snapshot().await.unwrap()).unwrap();
    let b_receipt = receipt(&stack, &rb.id).await;
    let b_native = stack.runtime(&b).await.session_id.unwrap();
    let b_pid = requests(&root, "session/prompt")
        .into_iter()
        .find(|r| r["params"]["sessionId"] == b_native)
        .unwrap()["_pid"]
        .as_u64()
        .unwrap();
    let (status, body) = stack.post_input(&a, "owner input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.wait_phase(&ra.id, "turn_running").await;
    let a_receipt = receipt(&stack, &ra.id).await;
    let ha = stack.harness(&ra.id);
    assert_eq!(a_receipt.0, "shared-key");
    assert_eq!(b_receipt.0, "shared-key");
    assert_ne!(a_receipt.1, b_receipt.1, "each owner has its own turn");
    ha.interrupt("selected owner only".into()).await.unwrap();
    ha.shutdown().await.unwrap();
    assert_eq!(receipt(&stack, &rb.id).await, b_receipt);
    assert_eq!(
        serde_json::to_value(hb.snapshot().await.unwrap()).unwrap(),
        before
    );
    assert!(
        requests(&root, "session/cancel")
            .iter()
            .all(|r| r["params"]["sessionId"] != b_native)
    );
    assert_eq!(
        requests(&root, "session/prompt")
            .iter()
            .filter(|r| r["params"]["sessionId"] == b_native)
            .count(),
        1
    );
    assert!(
        std::path::Path::new(&format!("/proc/{b_pid}")).exists(),
        "decoy process must remain alive"
    );
    stack.shutdown().await;
}
