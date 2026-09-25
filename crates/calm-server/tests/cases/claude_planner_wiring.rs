//! #1791 PR4: a Claude Planner through the production boot and the real routes, driving the fake
//! `claude`. See `claude_planner_stack_fixture.rs` for the server and its private root.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::session_projection_repo::{AgentProvider, WorkerSessionKind};
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

const BUDGET: Duration = Duration::from_secs(20);

async fn upload_png(stack: &Stack, card_id: &str) -> String {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(b"claude planner image");
    let response = stack
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/attachments"))
                .header("content-type", "image/png")
                .header("x-calm-actor", "user")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
            .unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["attachmentId"].as_str().expect("id").to_string()
}

/// The stored item of `item_type` for the card, if any.
async fn stored_item(stack: &Stack, card_id: &str, item_type: &str) -> Option<Value> {
    super::claude_planner_session_fixture::card_rows(stack.repo(), card_id, "item/completed")
        .await
        .into_iter()
        .find(|params| params["item"]["type"] == item_type)
}

pub(super) async fn wait_file(root: &Root, name: &str) {
    for _ in 0..400 {
        if root.read_fake(name).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the fake never wrote {name}");
}

/// Acceptance: create → image message with an MCP call → interrupt → steer refused → restart →
/// resume → next turn → track delete, all through the routes, on a server whose Codex daemon is
/// down.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_track_runs_through_the_real_routes() {
    let root = Root::new("mcp");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    assert_eq!(runtime.kind, WorkerSessionKind::SharedPlanner);
    assert_eq!(runtime.agent_provider, Some(AgentProvider::Claude));
    assert_eq!(
        stack.row_hash(&runtime.id).await,
        None,
        "no token before the first turn"
    );
    let thread = runtime.thread_id.clone().expect("a UUID thread");
    uuid::Uuid::parse_str(&thread).expect("the thread is a UUID");

    // Image message; the fake shakes hands with the kernel's MCP socket using its own token.
    let attachment = upload_png(&stack, &card_id).await;
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card_id}/planner/input"),
            Some(json!({"text": "look at this", "attachments": [attachment]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    // The outcome is recorded before `TurnCompleted` goes out; the run loop has persisted the
    // turn's items once it has seen that.
    stack.wait_phase(&runtime.id, "turn_completed").await;
    let line: Value = serde_json::from_str(
        root.read_fake("stdin")
            .expect("stdin")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let content = line["message"]["content"].as_array().expect("content");
    assert!(
        content.iter().any(|block| block["type"] == "image"
            && block["source"]["type"] == "base64"
            && block["source"]["media_type"] == "image/png"),
        "{line}"
    );
    let reply: Value =
        serde_json::from_str(root.read_fake("mcp_reply").expect("mcp reply").trim()).unwrap();
    assert!(
        reply.get("error").is_none(),
        "the minted token authenticates: {reply}"
    );
    let tool = stored_item(&stack, &card_id, "mcpToolCall")
        .await
        .expect("mcp tool item");
    assert_eq!(tool["item"]["tool"], "calm.report.write");
    let token = root.spawned_token();
    assert!(stack.token_authenticates(&token).await);
    assert!(root.wait_unmarked(&runtime.id, BUDGET).await.is_empty());

    // Interrupt a running turn; a queued message cannot be steered into it.
    root.set_scenario("hold");
    root.remove_fake("stdin");
    let (status, _) = stack.post_input(&card_id, "work for a while").await;
    assert_eq!(status, StatusCode::OK);
    wait_file(&root, "stdin").await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let (status, queued) = stack.post_input(&card_id, "and then this").await;
    assert_eq!(status, StatusCode::OK);
    let entry = queued["entry_id"].as_str().expect("entry id").to_string();
    let (status, refused) = stack
        .send(
            "POST",
            &format!("/api/cards/{card_id}/planner/input/{entry}/steer"),
            Some(json!({"if_entry_rev": 0})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused
            .to_string()
            .contains("cannot take messages into a running turn"),
        "{refused}"
    );
    root.set_scenario("exit");
    let (status, stopped) = stack
        .send(
            "POST",
            &format!("/api/cards/{card_id}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    let outcomes = stack.wait_outcomes(&card_id, 2).await;
    assert_eq!(outcomes[1]["status"], "interrupted", "{outcomes:?}");
    // The queued message runs as the next turn.
    let outcomes = stack.wait_outcomes(&card_id, 3).await;
    assert_eq!(outcomes[2]["status"], "completed", "{outcomes:?}");
    stack.wait_phase(&runtime.id, "turn_completed").await;

    // Restart: a fresh boot of the same root recovers the harness, which resumes the session.
    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    assert!(
        !stack.token_authenticates(&token).await,
        "boot revoked the old credential"
    );
    let (status, _) = stack.post_input(&card_id, "after the restart").await;
    assert_eq!(status, StatusCode::OK);
    let outcomes = stack.wait_outcomes(&card_id, 4).await;
    assert_eq!(outcomes[3]["status"], "completed", "{outcomes:?}");
    let argv = root.read_fake("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|a| *a == "--resume").expect("resumed");
    assert_eq!(argv[at + 1], thread);
    let new_token = root.spawned_token();
    assert_ne!(new_token, token, "the recovered harness minted its own");
    assert!(stack.token_authenticates(&new_token).await);

    // Delete: nothing of the Planner survives.
    root.spawn_marked_orphan(&runtime.id);
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track_id}"), None)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(root.marked_pids(&runtime.id).is_empty());
    assert!(root.instructions_files().is_empty());
}

/// Without `--claude-planner-config`: a Claude create is refused naming the flag, and a recovered
/// Claude harness keeps its queue and tells the reader why nothing is sent; nothing is spawned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_flag_create_is_refused_and_a_recovered_harness_refuses() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    stack.shutdown().await;

    let stack = Stack::boot_with(&root, false).await;
    let area = stack
        .repo()
        .track_get(&track_id)
        .await
        .expect("track read")
        .expect("track")
        .area_id;
    let (status, body) = stack
        .send(
            "POST",
            "/api/tracks",
            Some(json!({
                "planner_provider": "claude",
                "area_id": area,
                "title": "refused",
                "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("--claude-planner-config"),
        "{body}"
    );

    let harness = stack.harness(&runtime.id);
    let (status, _) = stack.post_input(&card_id, "hello?").await;
    assert_eq!(status, StatusCode::OK, "the message is queued");
    let mut block = None;
    for _ in 0..200 {
        block = harness.issuance_block().await;
        if block.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let block = block.expect("the reader is told why nothing is sent");
    assert!(block.contains("--claude-planner-config"), "{block}");
    assert!(root.read_fake("spawns").is_none(), "nothing was spawned");
    assert!(stack.outcomes(&card_id).await.is_empty());
}

/// §5.8: a Claude Planner offers no model choice: the catalog read answers `unavailable` for its
/// card or for `?provider=claude` (without asking Codex), a create naming a model is refused and a
/// model PUT on its card is a 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_surfaces_offer_no_choice_for_a_claude_planner() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    for uri in [
        format!("/api/models?card_id={card_id}"),
        "/api/models?provider=claude".to_string(),
    ] {
        let (status, body) = stack.send("GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["source"], "unavailable", "{uri}: {body}");
        assert_eq!(body["models"], json!([]), "{uri}: {body}");
    }
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card_id}/planner/model"),
            Some(json!({"model": "some-model", "reasoning_effort": null})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let area = stack
        .repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let (status, body) = stack
        .send(
            "POST",
            "/api/tracks",
            Some(json!({
                "planner_provider": "claude",
                "model": "some-model",
                "area_id": area,
                "title": "with a model",
                "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
