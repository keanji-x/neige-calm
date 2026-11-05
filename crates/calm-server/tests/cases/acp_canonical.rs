//! Canonical AGY cards exercise the production HTTP, driver and MCP owners.
use super::acp_compact::wait_phase;
use super::*;

async fn assert_canonical(stack: &Stack, card: &str) {
    let runtime = stack.runtime(card).await;
    let frame = runtime.handle_state_json.unwrap();
    assert!(frame.get("pending_queue").is_none());
    let receipts: Vec<String> =
        sqlx::query_scalar("SELECT input_json FROM acp_submissions WHERE worker_session_id=?")
            .bind(&runtime.id)
            .fetch_all(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert!(!receipts.is_empty());
    for stored in receipts {
        assert!(matches!(
            calm_server::db::sqlite::AcpSubmissionInput::decode(&stored).unwrap(),
            calm_server::db::sqlite::AcpSubmissionInput::Canonical(_)
        ));
    }
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM conversation_turns WHERE card_id=? LIMIT 1")
            .bind(card)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(provider, "antigravity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_cards_share_server_cancel_independently_and_revoke_archive() {
    let root = Root::new("unused");
    let stack = boot_as(&root, "antigravity", json!({
        "ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server", "ACP_FIXTURE_VERSION":"1.5.0", "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"
    }), "1.5.0").await;
    let (track, first) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let (_, second) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    std::fs::write(root.path().join("scenario"), "shared-hold").unwrap();
    for card in [&first, &second] {
        let (status, body) = stack.post_input(card, "canonical concurrent request").await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        while requests(&root, "session/prompt").len() < 2 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let first_native = stack.runtime(&first).await.session_id.unwrap();
    let second_native = stack.runtime(&second).await.session_id.unwrap();
    assert_ne!(first_native, second_native);
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{first}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stack.wait_outcomes(&first, 1).await[0]["status"],
        "interrupted"
    );
    assert_eq!(
        stack.wait_outcomes(&second, 1).await[0]["status"],
        "completed"
    );
    for card in [&first, &second] {
        wait_phase(&stack, &stack.runtime(card).await.id, "turn_completed").await;
        assert_canonical(&stack, card).await;
    }
    let token = |native: &str| {
        std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap()
    };
    let first_token = token(&first_native);
    let second_token = token(&second_native);
    assert!(stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let (status, body) = stack
        .send_with(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", uuid::Uuid::new_v4().to_string()),
            Some(json!({"provider":"antigravity", "text":"canonical Assistant reply"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let assistant = body["id"].as_str().unwrap();
    assert_eq!(
        stack.wait_outcomes(assistant, 1).await[0]["status"],
        "completed"
    );
    let runtime = stack.runtime(assistant).await;
    wait_phase(&stack, &runtime.id, "turn_completed").await;
    assert_canonical(&stack, assistant).await;
    let assistant_token = token(runtime.session_id.as_deref().unwrap());
    let stream = tokio::net::UnixStream::connect(stack.mcp_socket())
        .await
        .unwrap();
    let mut stream = tokio::io::BufReader::new(stream);
    let initialized = mcp_exchange(&mut stream, json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":"canonical-agy","version":"1"},
            "_meta":{"dev.neige/auth":{"token":assistant_token}}
        }
    })).await;
    assert!(initialized.get("result").is_some());
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track}/conversations/{assistant}/archive"),
            Some(json!({"archived":true})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!stack.token_authenticates(&assistant_token).await);
    assert!(stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    assert!(
        mcp_exchange(
            &mut stream,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        )
        .await
        .get("error")
        .is_some()
    );
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 3);
    assert!(
        prompts
            .iter()
            .all(|prompt| prompt["_pid"] == prompts[0]["_pid"])
    );
    stack.shutdown().await;
}
