//! Standalone Antigravity uses the existing managed ACP lifecycle and context readout.
use super::acp_compact::{turn, wait_phase};
use super::*;

async fn standalone(root: &Root) -> Stack {
    boot_as(
        root,
        "antigravity",
        json!({"ACP_FIXTURE_AGENT_NAME":"antigravity-acp", "ACP_FIXTURE_VERSION":"1.4.5",
            "ACP_FIXTURE_CLOSE":"0", "ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"}),
        "1.4.5",
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standalone_antigravity_context_reaches_both_roles_and_survives_restart() {
    let root = Root::new("unused");
    let stack = standalone(&root).await;
    let (track, planner) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    std::fs::write(root.path().join("scenario"), "usage").unwrap();
    assert_eq!(
        turn(&stack, &planner, "context reading", 1).await["status"],
        "completed"
    );
    let native = stack.runtime(&planner).await.session_id.unwrap();
    let (status, body) = stack
        .send_with(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", uuid::Uuid::new_v4().to_string()),
            Some(json!({"provider":"antigravity", "text":"Assistant context reading"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let assistant = body["id"].as_str().unwrap().to_owned();
    stack.wait_outcomes(&assistant, 1).await;
    wait_phase(
        &stack,
        &stack.runtime(&assistant).await.id,
        "turn_completed",
    )
    .await;
    let mut readings = Vec::new();
    for card in [&planner, &assistant] {
        let (status, run) = stack
            .send(
                "GET",
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let usage = run["token_usage"].clone();
        // The latest standard reading wins, even when occupancy decreased in this turn.
        assert_eq!(usage["used_tokens"], 24_100);
        assert_eq!(usage["context_window"], 200_000);
        let expected = (24_100.0 - 12_000.0) / (200_000.0 - 12_000.0) * 100.0;
        assert!((usage["percent"].as_f64().unwrap() - expected).abs() < 1e-9);
        assert!(usage.get("total_tokens").is_none());
        assert_eq!(run["manual_compact_supported"], false);
        readings.push(usage);
    }
    assert!(requests(&root, "session/close").is_empty());
    let argv = std::fs::read_to_string(root.path().join("argv.jsonl")).unwrap();
    for line in argv.lines() {
        assert_eq!(serde_json::from_str::<Value>(line).unwrap(), json!([]));
    }
    stack.shutdown().await;
    let stack = standalone(&root).await;
    for (card, usage) in [&planner, &assistant].into_iter().zip(readings) {
        let (status, run) = stack
            .send(
                "GET",
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["token_usage"], usage);
    }
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    assert_eq!(
        turn(&stack, &planner, "continue", 2).await["status"],
        "completed"
    );
    assert_eq!(
        stack.runtime(&planner).await.session_id.as_deref(),
        Some(native.as_str())
    );
    assert_eq!(requests(&root, "session/prompt").len(), 3);
    let prompts = requests(&root, "session/prompt");
    assert_ne!(prompts[0]["_pid"], prompts[2]["_pid"]);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standalone_antigravity_refuses_manual_compaction_before_dispatch() {
    let root = Root::new("unused");
    let stack = standalone(&root).await;
    let (_, card) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    turn(&stack, &card, "remember context", 1).await;
    let before = requests(&root, "session/prompt");
    let (status, _) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/compact"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(requests(&root, "session/prompt"), before);
    stack.shutdown().await;
}
