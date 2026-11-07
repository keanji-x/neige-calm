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
async fn canonical_antigravity_processes_cancel_independently_and_revoke_turn_authority() {
    let root = Root::new("unused");
    let stack = boot_as(
        &root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"antigravity-acp", "ACP_FIXTURE_VERSION":"1.4.5",
            "ACP_FIXTURE_CLOSE":"0", "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"
        }),
        "1.4.5",
    )
    .await;
    let (track, first) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let (_, second) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
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
    let token = |native: &str| {
        std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap()
    };
    let first_token = token(&first_native);
    let second_token = token(&second_native);
    assert!(stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    let stream = tokio::net::UnixStream::connect(stack.mcp_socket())
        .await
        .unwrap();
    let mut stream = tokio::io::BufReader::new(stream);
    let initialized = mcp_exchange(&mut stream, json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
        "protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":"canonical-acp","version":"1"},
        "_meta":{"dev.neige/auth":{"token":first_token}}
    }})).await;
    assert!(initialized.get("result").is_some());
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
    wait_phase(&stack, &stack.runtime(&first).await.id, "turn_completed").await;
    assert!(!stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    assert!(stack.outcomes(&second).await.is_empty());
    assert!(
        mcp_exchange(
            &mut stream,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        )
        .await
        .get("error")
        .is_some()
    );
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{second}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stack.wait_outcomes(&second, 1).await[0]["status"],
        "interrupted"
    );
    for card in [&first, &second] {
        wait_phase(&stack, &stack.runtime(card).await.id, "turn_completed").await;
        assert_canonical(&stack, card).await;
    }
    assert!(!stack.token_authenticates(&second_token).await);
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
    assert!(
        !stack
            .token_authenticates(&token(runtime.session_id.as_deref().unwrap()))
            .await
    );
    assert_eq!(
        stack
            .send(
                "PATCH",
                &format!("/api/tracks/{track}/conversations/{assistant}/archive"),
                Some(json!({"archived":true}))
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 3);
    let processes: std::collections::HashSet<_> = prompts
        .iter()
        .map(|p| p["_pid"].as_u64().unwrap())
        .collect();
    assert_eq!(processes.len(), 3);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_full_approves_permission_in_its_own_turn() {
    let root = Root::new("unused");
    let stack = boot_as_with_creation_mode(
        &root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"antigravity-acp", "ACP_FIXTURE_CLOSE":"0", "ACP_FIXTURE_VERSION":"1.4.5"
        }),
        "1.4.5",
        Some("full"),
    )
    .await;
    let (_, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    std::fs::write(root.path().join("scenario"), "ask").unwrap();
    let (status, body) = stack.post_input(&card, "use a tool").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stack.wait_outcomes(&card, 1).await[0]["status"],
        "completed"
    );
    let log = std::fs::read_to_string(root.path().join("permission-log.jsonl")).unwrap();
    let reply: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(
        reply["outcome"],
        json!({"outcome":"selected", "optionId":"once"})
    );
    stack.shutdown().await;
}

/// Independent native processes serve cards with different modes. Full must not approve a
/// concurrent Ask card, nor turn a Never denial into a silent successful completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_permission_modes_are_isolated_and_denials_explain_early_stop() {
    use calm_server::event::Event;
    let root = Root::new("unused");
    let stack = boot_as(
        &root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"antigravity-acp", "ACP_FIXTURE_CLOSE":"0", "ACP_FIXTURE_VERSION":"1.4.5"
        }),
        "1.4.5",
    )
    .await;
    let (ask_track, ask) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let (_, full) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let (_, never) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    for (card, mode) in [(&ask, "ask"), (&full, "full"), (&never, "never")] {
        let (status, body) = stack
            .send(
                "PUT",
                &format!("/api/cards/{card}/planner/permission-mode"),
                Some(json!({"permission_mode":mode})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    std::fs::write(root.path().join("scenario"), "antigravity-permissions").unwrap();
    let (status, body) = stack.post_input(&ask, "ask before operation").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ask_id = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let events = stack
                .repo()
                .events_for_track(&ask_track, &["ask.requested"], None)
                .await
                .unwrap();
            if let Some(event) = events
                .into_iter()
                .find(|event| matches!(event.event, Event::AskRequested { .. }))
            {
                break event.id;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    for card in [&full, &never] {
        let (status, body) = stack.post_input(card, "operation").await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let outcome = stack.wait_outcomes(&full, 1).await[0].clone();
    assert_eq!(outcome["status"], "completed", "{outcome}");
    let outcome = stack.wait_outcomes(&never, 1).await[0].clone();
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("`never`"),
        "{outcome}"
    );
    // The Ask remains pending while the independent Full process has completed.
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/tracks/{ask_track}/asks/{ask_id}/answer"),
            Some(json!({"answers":[{"option":1}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let outcome = stack.wait_outcomes(&ask, 1).await[0].clone();
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert_eq!(
        outcome["error"]["code"], "acp_permission_rejected",
        "{outcome}"
    );
    for (card, code) in [
        (&ask, "acp_permission_rejected"),
        (&never, "acp_permission_denied"),
    ] {
        wait_phase(&stack, &stack.runtime(card).await.id, "turn_completed").await;
        assert_eq!(stack.outcomes(card).await[0]["error"]["code"], code);
    }
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 3);
    assert_eq!(
        prompts
            .iter()
            .map(|p| p["_pid"].as_u64().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_antigravity_ask_approval_continues_and_cancel_stays_interrupted() {
    let root = Root::new("unused");
    let stack = boot_as_with_creation_mode(
        &root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME":"antigravity-acp", "ACP_FIXTURE_CLOSE":"0", "ACP_FIXTURE_VERSION":"1.4.5"
        }),
        "1.4.5",
        Some("ask"),
    )
    .await;
    let (track, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    std::fs::write(root.path().join("scenario"), "antigravity-permissions").unwrap();
    for n in 1..=2 {
        let (status, body) = stack
            .post_input(&card, "operation requiring approval")
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ask_id = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let events = stack
                    .repo()
                    .events_for_track(&track, &["ask.requested"], None)
                    .await
                    .unwrap();
                if events.len() >= n {
                    break events[n - 1].id;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        if n == 1 {
            let (status, body) = stack
                .send(
                    "POST",
                    &format!("/api/tracks/{track}/asks/{ask_id}/answer"),
                    Some(json!({"answers":[{"option":0}]})),
                )
                .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            assert_eq!(
                stack.wait_outcomes(&card, n).await[n - 1]["status"],
                "completed"
            );
        } else {
            let (status, body) = stack
                .send(
                    "POST",
                    &format!("/api/cards/{card}/planner/interrupt"),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(
                stack.wait_outcomes(&card, n).await[n - 1]["status"],
                "interrupted"
            );
        }
        wait_phase(&stack, &stack.runtime(&card).await.id, "turn_completed").await;
    }
    stack.shutdown().await;
}
