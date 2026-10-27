//! Mixed-provider switching through authenticated HTTP and canonical Claude execution.
use super::*;

#[tokio::test]
async fn planner_switch_claude_assistant_next_input() {
    let mut p = Pair::with_provider("claude").await;
    // Exact native acknowledgement makes the completed Assistant eligible for
    // replacement without manufacturing a Human Keep decision.
    let (_, before) = assistant_canonical::carrier(&p.fixture, &p.cards[1]).await;
    let attempt = &before.canonical_inputs.attempts[0];
    assert!(matches!(
        attempt.delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(before.canonical_inputs.receipts.is_empty());
    claude_creation_tests::assistant::wait_frame(&p.fixture, &p.cards[1], |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.referenced_submissions().is_empty()
    })
    .await;
    let request = http::body(&p);
    let (status, receipt) = http::submit(&mut p, request).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let detail = http::detail(&p).await;
    for (index, key, absent, provider) in [
        (0, "assistant_provider", "planner_provider", "codex"),
        (1, "planner_provider", "assistant_provider", "claude"),
    ] {
        let card = detail["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == p.cards[index])
            .unwrap();
        assert_eq!(card["payload"][key], provider);
        assert!(card["payload"].get(absent).is_none());
    }
    let sent = post(
        &p.app,
        &p.cookie,
        &format!("/api/cards/{}/planner/input", p.cards[1]),
        "claude-next",
        json!({"text":"Switched Planner Claude input"}),
    )
    .await;
    assert_ne!(sent["worker_session_id"], p.workers[1]);
    let frame = claude_creation_tests::assistant::wait_frame(&p.fixture, &p.cards[1], |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 1
    })
    .await;
    assert!(frame.canonical_inputs.pending.is_empty());
    let worker = p
        .fixture
        .repo
        .session_get(&sent["worker_session_id"].as_str().unwrap().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        worker.provider,
        calm_types::worker::WorkerProviderKind::Claude
    );
    assert!(
        claude_creation_tests::assistant::read(&p.fixture, "stdin")
            .contains("Switched Planner Claude input")
    );
    assert_eq!(
        claude_creation_tests::assistant::read(&p.fixture, "spawns")
            .lines()
            .count(),
        2
    );
    p.close().await;
}
