//! Managed ACP acceptance through the production boot, REST routes and Harness.
#[allow(dead_code)]
#[path = "cases/claude_planner_session_fixture.rs"]
mod claude_planner_session_fixture;
#[allow(dead_code)]
#[path = "cases/claude_planner_stack_fixture.rs"]
mod stack_fixture;

use axum::http::StatusCode;
use serde_json::{Value, json};
use stack_fixture::{Root, Stack};
use std::time::Duration;

const PEER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/acp_planner/agent.py"
);
fn requests(root: &Root, method: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|frame| frame["method"] == method)
        .collect()
}
async fn boot(root: &Root) -> Stack {
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let config_path = root.path().join("acp.json");
    std::fs::write(&config_path,json!({"agents":[{"provider":"opencode","command":"/usr/bin/python3","args":["-u",PEER],"env":{"ACP_FIXTURE_ROOT":root.path()},"expected_agent_name":"Fixture ACP","expected_agent_version":"1"}]}).to_string()).unwrap();
    let mut config = root.config(false);
    config.acp_planner_config = Some(config_path);
    Stack::boot_config(&config).await
}
async fn create(stack: &Stack) -> (String, String) {
    stack
        .create_claude_track_with(json!({"planner_provider":"opencode","title":"Managed ACP"}))
        .await
}
async fn turn(stack: &Stack, card: &str, text: &str, n: usize) -> Value {
    let (status, body) = stack.post_input(card, text).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = stack.wait_outcomes(card, n).await[n - 1].clone();
    let runtime = stack.runtime(card).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
    outcome
}
async fn wait_file(root: &Root, name: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.path().join(name).exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("peer file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_runs_two_turns_and_loads_the_same_native_session_after_restart() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    assert_eq!(
        runtime.agent_provider,
        Some(calm_server::session_projection_repo::AgentProvider::OpenCode)
    );
    assert!(runtime.session_id.is_none());
    assert!(requests(&root, "session/prompt").is_empty());
    assert_eq!(
        turn(&stack, &card, "first explicit input", 1).await["status"],
        "completed"
    );
    let native = stack
        .runtime(&card)
        .await
        .session_id
        .expect("native binding");
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter()
            .any(|row| row["item"]["text"] == "reply: User says:\nfirst explicit input"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row["item"]["result"]["content"][0]["text"] == "native tool output"),
        "{rows:?}"
    );
    assert_eq!(
        turn(&stack, &card, "second explicit input", 2).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    assert_eq!(requests(&root, "session/new").len(), 1);
    stack.shutdown().await;
    let stack = boot(&root).await;
    assert_eq!(
        stack.runtime(&card).await.session_id.as_deref(),
        Some(native.as_str())
    );
    assert_eq!(
        requests(&root, "session/prompt").len(),
        2,
        "boot and replay cannot submit"
    );
    assert_eq!(
        turn(&stack, &card, "after restart", 3).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 3);
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter()
            .filter_map(|row| row["item"]["text"].as_str())
            .all(|text| !text.contains("historic reply")),
        "loaded history must not be attributed to a fresh turn: {rows:?}"
    );
    assert!(
        requests(&root, "session/load")
            .iter()
            .all(|request| request["params"]["sessionId"] == native)
    );
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track}"), None)
        .await;
    assert!(status.is_success(), "{status}: {body}");
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_permission_requests_use_never_without_creating_asks() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "permission").unwrap();
    assert_eq!(
        turn(&stack, &card, "request permission", 1).await["status"],
        "interrupted"
    );
    wait_file(&root, "permission-reply.json").await;
    let reply: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("permission-reply.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(reply["result"]["outcome"], json!({"outcome":"cancelled"}));
    assert!(
        stack
            .repo()
            .events_for_track(&track, &["ask.requested"], None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_lost_prompt_response_stays_fenced_after_restart_without_resend() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "lost").unwrap();
    let outcome = turn(&stack, &card, "perform once", 1).await;
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
    let stack = boot(&root).await;
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
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "unknown admission cannot be retried"
    );
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap()
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_uses_declared_configuration_keys_and_authenticates_mcp() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "mcp").unwrap();
    assert_eq!(turn(&stack, &card, "first", 1).await["status"], "completed");
    let reply: Value =
        serde_json::from_str(&std::fs::read_to_string(root.path().join("mcp-reply.json")).unwrap())
            .unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    let (status, catalog) = stack
        .send("GET", &format!("/api/models?card_id={card}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["default_source"], "acp_session");
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["model"] == "fixture/model-b")
    );
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/model"),
            Some(json!({"model":"fixture/model-b","reasoning_effort":"deep"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        turn(&stack, &card, "selected", 2).await["status"],
        "completed"
    );
    let changes = requests(&root, "session/set_config_option");
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["params"]["configId"], "declared-model-key");
    assert_eq!(changes[1]["params"]["configId"], "declared-effort-key");
    for line in std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
    {
        let presence: Value = serde_json::from_str(line).unwrap();
        assert_eq!(presence["NEIGE_MCP_DAEMON_TOKEN"], false);
        assert_eq!(presence["NEIGE_MCP_TOKEN"], false);
        assert_eq!(presence["ACP_AMBIENT_SENTINEL"], false);
    }
    stack.shutdown().await;
}
