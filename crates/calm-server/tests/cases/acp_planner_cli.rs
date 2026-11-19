//! A resident OpenCode process holds no ambient card credentials.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_opencode_accepts_mcp_calls_only_from_the_credential_owner() {
    let root = Root::new("unused");
    let stack = boot_with(
        &root,
        json!({"ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"}),
        "2.1.3",
    )
    .await;
    let (track, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "mcp-origin").unwrap();
    assert_eq!(
        turn(&stack, &card, "check Neige tools", 1).await["status"],
        "completed"
    );

    let launches = std::fs::read_to_string(root.path().join("environment.jsonl")).unwrap();
    let resident = launches
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|launch| launch["opencode_config_content"].is_string())
        .expect("resident launch");
    assert_eq!(resident["NEIGE_MCP_TOKEN"], false);
    assert_eq!(resident["NEIGE_MCP_SOCKET"], false);

    let replies: [Value; 4] = serde_json::from_slice(
        &std::fs::read(root.path().join("mcp-origin-results.json")).unwrap(),
    )
    .unwrap();
    assert!(replies[0].get("result").is_some(), "{replies:?}");
    assert_ne!(replies[0]["result"]["isError"], true, "{replies:?}");
    for reply in &replies[1..3] {
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            reply["result"]["structuredContent"]["code"], -32602,
            "{reply}"
        );
    }
    assert_eq!(replies[3]["error"]["code"], -32600, "{replies:?}");

    let native = stack.runtime(&card).await.session_id.unwrap();
    let grant_path = root.path().join(format!("{native}.test-token"));
    let token = std::fs::read_to_string(&grant_path).unwrap();
    let (mut reader, mut writer) = crate::support::mcp::connect(stack.mcp_socket()).await;
    crate::support::mcp::handshake(&mut reader, &mut writer, &token).await;
    crate::support::mcp::send_frame(&mut writer, json!({
        "jsonrpc":"2.0", "id":2, "method":"tools/call",
        "params":{"name":"neige_track_status","arguments":{},"_meta":{"ai.opencode/sessionID":native}},
    })).await;
    let idle_reply = crate::support::mcp::recv_frame(&mut reader).await;
    assert!(idle_reply.get("result").is_some(), "{idle_reply}");
    assert_ne!(idle_reply["result"]["isError"], true, "{idle_reply}");
    assert_eq!(
        turn(&stack, &card, "second", 2).await["status"],
        "completed"
    );
    assert!(stack.token_authenticates(&token).await);
    assert_eq!(std::fs::read_to_string(grant_path).unwrap(), token);
    calm_server::acp_planner::config::sweep_track(
        stack.repo(),
        &stack.state.planner_wiring().acp,
        &track,
    )
    .await
    .unwrap();
    assert!(!stack.token_authenticates(&token).await);
    crate::support::mcp::send_frame(
        &mut writer,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{}
        }),
    )
    .await;
    let revoked = crate::support::mcp::recv_frame(&mut reader).await;
    assert_eq!(
        revoked["error"]["code"],
        calm_server::mcp_server::handshake::TOKEN_NOT_RECOGNIZED_CODE
    );

    assert_eq!(
        turn(&stack, &card, "after generation retirement", 3).await["status"],
        "completed"
    );
    assert_eq!(
        stack.runtime(&card).await.session_id.as_deref(),
        Some(native.as_str())
    );
    let fresh = std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap();
    assert_ne!(fresh, token);
    assert!(stack.token_authenticates(&fresh).await);
    assert!(!stack.token_authenticates(&token).await);
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts[0]["_pid"], prompts[1]["_pid"]);
    assert_ne!(prompts[1]["_pid"], prompts[2]["_pid"]);
    let (mut reader, mut writer) = crate::support::mcp::connect(stack.mcp_socket()).await;
    crate::support::mcp::handshake(&mut reader, &mut writer, &fresh).await;
    let (status, body) = stack.reset(&card).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!stack.token_authenticates(&fresh).await);
    crate::support::mcp::send_frame(
        &mut writer,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}
        }),
    )
    .await;
    let replaced = crate::support::mcp::recv_frame(&mut reader).await;
    assert_eq!(
        replaced["error"]["code"],
        calm_server::mcp_server::handshake::TOKEN_NOT_RECOGNIZED_CODE
    );

    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retiring_resident_opencode_workspace_preserves_other_workspace_execution() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, first) = create(&stack).await;
    let (sibling_track, sibling) = create(&stack).await;
    assert_ne!(track, sibling_track);
    assert_eq!(
        turn(&stack, &first, "first", 1).await["status"],
        "completed"
    );
    std::fs::write(root.path().join("scenario"), "resident-drain-sibling").unwrap();
    assert_eq!(
        stack.post_input(&sibling, "sibling").await.0,
        StatusCode::OK
    );
    wait_file(&root, "resident-sibling-held").await;
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 2);
    assert_ne!(prompts[0]["_pid"], prompts[1]["_pid"]);
    let pool = stack.repo().sqlite_db().unwrap();
    let first_worker = stack.runtime(&first).await.id;
    let sibling_worker = stack.runtime(&sibling).await.id;
    let first_generations =
        calm_truth::db::sqlite::acp_resident::for_worker(pool.read_pool(), &first_worker)
            .await
            .unwrap();
    let sibling_generations =
        calm_truth::db::sqlite::acp_resident::for_worker(pool.read_pool(), &sibling_worker)
            .await
            .unwrap();
    assert_eq!(first_generations.len(), 1);
    assert_eq!(sibling_generations.len(), 1);
    assert_ne!(first_generations, sibling_generations);
    let first_generation =
        workspace_runtime::execution::ExecutionId::try_from(first_generations[0].clone()).unwrap();
    let wiring = stack.state.planner_wiring();
    let sweep = || calm_server::acp_planner::config::sweep_track(stack.repo(), &wiring.acp, &track);
    sweep().await.unwrap();
    let settled = calm_server::provider_execution::settlement(&pool, &first_generation)
        .await
        .unwrap()
        .expect("target generation has executor settlement");
    assert!(
        settled.descendants_complete(),
        "target workspace descendants must have exited: {settled:?}"
    );
    assert!(
        calm_truth::db::sqlite::acp_resident::for_worker(pool.read_pool(), &first_worker)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        calm_truth::db::sqlite::acp_resident::for_worker(pool.read_pool(), &sibling_worker)
            .await
            .unwrap(),
        sibling_generations
    );
    assert!(stack.outcomes(&sibling).await.is_empty());
    assert!(requests(&root, "session/cancel").is_empty());
    std::fs::write(root.path().join("release-resident-sibling"), "").unwrap();
    assert_eq!(
        stack.wait_outcomes(&sibling, 1).await[0]["status"],
        "completed"
    );
    sweep().await.unwrap();
    assert!(requests(&root, "session/cancel").is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_opencode_keep_uses_later_exact_drain_after_a_sibling_finishes() {
    keep_after_resident_sibling_drain(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_opencode_keep_uses_durable_unknown_after_terminal_write_refused() {
    keep_after_resident_sibling_drain(true).await;
}

async fn keep_after_resident_sibling_drain(refuse_completion: bool) {
    use axum::http::Request;
    use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, first) = stack
        .create_canonical_track_with(json!({"planner_provider":"opencode"}))
        .await;
    super::acp_compact::turn(&stack, &first, "first", 1).await;
    std::fs::write(root.path().join("scenario"), "resident-drain-sibling").unwrap();
    let sibling = super::acp_compact::create_assistant(&stack, &track, "opencode", "sibling").await;
    wait_file(&root, "resident-sibling-held").await;
    let initial = requests(&root, "session/prompt");
    assert_eq!(initial.len(), 2);
    assert_eq!(initial[0]["_pid"], initial[1]["_pid"]);
    let pool = stack.repo().sqlite_db().unwrap();
    if refuse_completion {
        let runtime = stack.runtime(&first).await;
        let worker = runtime.id;
        pool.writer()
            .execute(sqlx::query(&format!(
                concat!(
                    "CREATE TRIGGER refuse_resident_end BEFORE UPDATE ON acp_submissions ",
                    "WHEN NEW.worker_session_id='{}' AND NEW.state='completed' ",
                    "BEGIN SELECT RAISE(ABORT,'fixture terminal write refusal'); END"
                ),
                worker
            )))
            .await
            .unwrap();
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
    } else {
        std::fs::write(root.path().join("scenario"), "shared-unknown").unwrap();
    }
    assert_eq!(
        stack.post_input(&first, "lost result").await.0,
        StatusCode::OK
    );
    assert_eq!(stack.wait_outcomes(&first, 2).await[1]["status"], "failed");
    if refuse_completion {
        pool.writer()
            .execute(sqlx::query("DROP TRIGGER refuse_resident_end"))
            .await
            .unwrap();
    }
    let runtime = stack.runtime(&first).await;
    let snapshot: CanonicalHarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    let attempt = snapshot
        .canonical_inputs
        .attempts
        .last()
        .expect("unknown attempt");
    let (status, body) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{first}/conversation/input/keep"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", "opencode-keep-after-drain"),
            Some(
                json!({"carrier_id":runtime.id,"submission_id":attempt.attempt.submission_id,
            "attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["waiting_for_settlement"], true);
    assert!(stack.outcomes(&sibling).await.is_empty());

    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    std::fs::write(root.path().join("release-resident-sibling"), "").unwrap();
    assert_eq!(
        stack.wait_outcomes(&sibling, 1).await[0]["status"],
        "completed"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if !calm_server::db::sqlite::acp_submission_unresolved(pool.read_pool(), &runtime.id)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    super::acp_compact::turn(&stack, &first, "after Keep", 3).await;
    assert_eq!(
        requests(&root, "session/prompt").len(),
        4,
        "Keep may not replay the original"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_opencode_abandon_accepts_exact_drained_generation() {
    use axum::http::Request;
    use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"opencode"}))
        .await;
    std::fs::write(root.path().join("scenario"), "shared-unknown").unwrap();
    assert_eq!(
        stack.post_input(&card, "lost result").await.0,
        StatusCode::OK
    );
    assert_eq!(stack.wait_outcomes(&card, 1).await[0]["status"], "failed");
    let runtime = stack.runtime(&card).await;
    let snapshot: CanonicalHarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    let attempt = snapshot
        .canonical_inputs
        .attempts
        .last()
        .expect("unknown attempt");
    let (status, body) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card}/conversation/abandon"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", "opencode-abandon-resident"),
            Some(
                json!({"carrier_id":runtime.id,"submission_id":attempt.attempt.submission_id,
            "attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["old_carrier_id"], runtime.id);
    assert_ne!(body["carrier_id"], runtime.id);
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "Abandon may not replay the original"
    );
    stack.shutdown().await;
}
