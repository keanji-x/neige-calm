//! Shared canonical maintenance goes through the same human REST command as other backends.
use super::acp_compact::{compact, human_state, snapshot, turn, wait_phase};
use super::*;

async fn agy(root: &Root) -> Stack {
    boot_as(
        root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server", "ACP_FIXTURE_VERSION":"1.5.0",
            "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"
        }),
        "1.5.0",
    )
    .await
}
async fn planner(stack: &Stack) -> (String, String) {
    stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await
}
async fn completed(stack: &Stack, card: &str) -> Value {
    wait_phase(stack, &stack.runtime(card).await.id, "turn_completed").await;
    snapshot(stack, card).await
}
async fn receipts(stack: &Stack, worker: &str) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as("SELECT client_id,input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=? ORDER BY client_id")
        .bind(worker).fetch_all(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap()
}
fn compact_requests(root: &Root) -> Vec<Value> {
    requests(root, "session/prompt")
        .into_iter()
        .filter(|r| r["params"]["prompt"] == json!([{"type":"text","text":"/compact"}]))
        .collect()
}

fn owned_closes(root: &Root) -> Vec<Value> {
    let sessions: std::collections::HashSet<_> = requests(root, "session/prompt")
        .into_iter()
        .map(|r| r["params"]["sessionId"].as_str().unwrap().to_owned())
        .collect();
    requests(root, "session/close")
        .into_iter()
        .filter(|r| sessions.contains(r["params"]["sessionId"].as_str().unwrap()))
        .collect()
}
fn assert_one_prompt_process(root: &Root) {
    let pids: std::collections::HashSet<_> = requests(root, "session/prompt")
        .into_iter()
        .map(|r| r["_pid"].as_u64().unwrap())
        .collect();
    assert_eq!(pids.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_compact_preserves_inputs_receipts_and_session_grant() {
    let root = Root::new("unused");
    let stack = agy(&root).await;
    let (track, card) = planner(&stack).await;
    turn(&stack, &card, "remember context", 1).await;
    let (status, body) = stack
        .send_with(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", uuid::Uuid::new_v4().to_string()),
            Some(json!({"provider":"antigravity", "text":"Assistant context"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let assistant = body["id"].as_str().unwrap();
    stack.wait_outcomes(assistant, 1).await;
    completed(&stack, assistant).await;
    for card in [&card, assistant] {
        let runtime = stack.runtime(card).await;
        let native = runtime.session_id.clone().unwrap();
        let token =
            std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap();
        let before = snapshot(&stack, card).await;
        let human = human_state(&stack, card).await;
        let journal = receipts(&stack, &runtime.id).await;
        let (status, run) = stack
            .send(
                "GET",
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["manual_compact_supported"], true);
        std::fs::write(root.path().join("scenario"), "shared-compact-success").unwrap();
        let (status, body) = compact(&stack, card).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let after = completed(&stack, card).await;
        assert_eq!(after["compact_outcome"]["status"], "completed", "{after}");
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        assert_eq!(after["canonical_inputs"], before["canonical_inputs"]);
        assert_eq!(human_state(&stack, card).await, human);
        assert_eq!(receipts(&stack, &runtime.id).await, journal);
        assert!(stack.token_authenticates(&token).await);
        assert_eq!(
            stack.runtime(card).await.session_id.as_deref(),
            Some(native.as_str())
        );
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        assert_eq!(
            turn(&stack, card, "continue after compaction", 2).await["status"],
            "completed"
        );
    }
    assert_eq!(compact_requests(&root).len(), 2);
    assert_one_prompt_process(&root);
    assert!(owned_closes(&root).is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_compact_errors_cannot_complete_and_revoke_only_owner() {
    for scenario in [
        "shared-compact-error",
        "shared-compact-invalid",
        "shared-compact-refusal",
        "shared-compact-client-request",
        "shared-compact-wrong-session",
    ] {
        let root = Root::new("unused");
        let stack = agy(&root).await;
        let (_, first) = planner(&stack).await;
        let (_, second) = planner(&stack).await;
        turn(&stack, &first, "first", 1).await;
        turn(&stack, &second, "second", 1).await;
        let token = |native: &str| {
            std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap()
        };
        let first_native = stack.runtime(&first).await.session_id.unwrap();
        let second_token = token(&stack.runtime(&second).await.session_id.unwrap());
        let first_token = token(&first_native);
        std::fs::write(root.path().join("scenario"), scenario).unwrap();
        let (status, body) = compact(&stack, &first).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        wait_phase(&stack, &stack.runtime(&first).await.id, "wedged").await;
        assert_ne!(
            snapshot(&stack, &first).await["compact_outcome"]["status"],
            "completed"
        );
        assert!(!stack.token_authenticates(&first_token).await, "{scenario}");
        assert!(stack.token_authenticates(&second_token).await, "{scenario}");
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        assert_eq!(
            turn(&stack, &second, "sibling continues", 2).await["status"],
            "completed"
        );
        assert_eq!(compact_requests(&root).len(), 1);
        assert_one_prompt_process(&root);
        let closes = owned_closes(&root);
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0]["params"]["sessionId"], first_native);
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_compact_empty_menu_refuses_before_write() {
    let root = Root::new("unused");
    let stack = agy(&root).await;
    let (_, card) = planner(&stack).await;
    turn(&stack, &card, "first", 1).await;
    std::fs::write(root.path().join("scenario"), "shared-menu-empty").unwrap();
    turn(&stack, &card, "menu changed", 2).await;
    let before = snapshot(&stack, &card).await;
    assert!(!compact(&stack, &card).await.0.is_success());
    assert!(compact_requests(&root).is_empty());
    assert_eq!(
        snapshot(&stack, &card).await["last_turn_id"],
        before["last_turn_id"]
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_two_compacts_route_independent_results() {
    let root = Root::new("unused");
    let stack = agy(&root).await;
    let (_, first) = planner(&stack).await;
    let (_, second) = planner(&stack).await;
    turn(&stack, &first, "first", 1).await;
    turn(&stack, &second, "second", 1).await;
    std::fs::write(root.path().join("scenario"), "shared-compact-wait").unwrap();
    for card in [&first, &second] {
        let (status, body) = compact(&stack, card).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let first_native = stack.runtime(&first).await.session_id.unwrap();
    std::fs::write(root.path().join(format!("release-{first_native}")), "").unwrap();
    assert_eq!(
        completed(&stack, &first).await["compact_outcome"]["status"],
        "completed"
    );
    assert_eq!(
        snapshot(&stack, &second).await["compact_outcome"]["status"],
        "started"
    );
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    assert_eq!(
        turn(&stack, &first, "first continues while second compacts", 2).await["status"],
        "completed"
    );
    let second_native = stack.runtime(&second).await.session_id.unwrap();
    std::fs::write(root.path().join(format!("release-{second_native}")), "").unwrap();
    assert_eq!(
        completed(&stack, &second).await["compact_outcome"]["status"],
        "completed"
    );
    assert_eq!(compact_requests(&root).len(), 2);
    assert_one_prompt_process(&root);
    assert!(owned_closes(&root).is_empty());
    stack.shutdown().await;
}
