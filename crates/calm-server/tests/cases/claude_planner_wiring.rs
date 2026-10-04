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
    assert_eq!(tool["item"]["tool"], "neige.report.commit");
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

/// #1981 S1: a pinned CLI that reports another version is Claude's refusal, so the reader is told
/// the reason now rather than a generic wait after the silence budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_version_mismatch_tells_the_reader_why_nothing_is_sent() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    std::fs::write(root.fake_dir().join("version"), "2.1.279").expect("version");

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
    assert!(block.starts_with("claude will not start a turn"), "{block}");
    assert!(
        block.contains(r#"--version reports "2.1.279", the config pins "2.1.280""#),
        "{block}"
    );
    assert!(stack.outcomes(&card_id).await.is_empty());
    stack.shutdown().await;
}

/// #1981 S1: a `--version` that gave no answer establishes no mismatch, so the failed attempt
/// stays transient: the reader is not told, and the retry keeps its short pace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_version_check_without_an_answer_stays_transient() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    root.remove_fake("claude");

    let harness = stack.harness(&runtime.id);
    let (status, _) = stack.post_input(&card_id, "hello?").await;
    assert_eq!(status, StatusCode::OK, "the message is queued");
    for _ in 0..200 {
        if harness.refused_issuances_for_test() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(harness.refused_issuances_for_test() > 0, "the turn failed");
    // A refusal sets the block before the attempt is counted.
    assert_eq!(harness.issuance_block().await, None);
    stack.shutdown().await;
}

/// #1981 S3: a Claude Planner opened by the production start path checks the server's one seal
/// registry, the one the deletion routes seal through. A sealed thread starts no turn; once the
/// seal is rolled back, the same queued message runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_sealed_in_the_server_registry_starts_no_claude_turn() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let thread = runtime.thread_id.clone().expect("a UUID thread");
    stack.state.thread_seals().seal_for_deletion(&thread);

    let harness = stack.harness(&runtime.id);
    let (status, body) = stack.post_input(&card_id, "hello?").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for _ in 0..200 {
        if harness.refused_issuances_for_test() > 0 || root.read_fake("stdin").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(root.read_fake("stdin"), None, "a sealed thread ran a turn");
    assert!(
        harness.refused_issuances_for_test() > 0,
        "the turn was refused"
    );

    stack.state.thread_seals().unseal_after_rollback(&thread);
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    stack.shutdown().await;
}

/// #1830 T3: on an attached track the Claude Planner's turn runs in the track worktree, and its
/// Edit/Write rules are confined to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_planner_turn_runs_in_the_track_worktree() {
    let root = Root::new("exit");
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ][..],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    let checkout = checkout.canonicalize().expect("canonical checkout");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack
        .create_claude_track_with(json!({"cwd": checkout, "attach_folder": true}))
        .await;
    let worktree = checkout
        .join(".claude/worktrees")
        .join(format!("track-{track_id}"));
    assert!(worktree.is_dir(), "premise: the track worktree exists");

    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(
        root.read_fake("pwd").expect("pwd").trim(),
        worktree.to_str().expect("utf-8"),
        "the turn runs in the track worktree"
    );
    let argv = root.read_fake("argv").expect("argv");
    let rule = format!("Edit(/{}/**)", worktree.display());
    assert!(
        argv.lines().any(|arg| arg.split(' ').any(|r| r == rule)),
        "Edit is confined to the worktree ({rule}): {argv}"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_watchdog_timeout_reason_survives_a_server_restart() {
    use calm_server::harness::{HarnessConfig, HarnessState};
    use std::time::Instant;
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let session = stack.runtime(&card_id).await;
    let (status, body) = stack.post_input(&card_id, "work until interrupted").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "stdin").await;
    stack.wait_phase(&session.id, "turn_running").await;
    let harness = stack.harness(&session.id);
    let HarnessState::TurnRunning { turn_id, .. } = harness.state_for_test().await else {
        panic!("the fixture must still be running");
    };
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id,
            started_at: Instant::now()
                - HarnessConfig::default().max_turn_duration
                - Duration::from_secs(1),
        })
        .await;
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "interrupted");
    assert_eq!(
        outcomes[0]["harness_interruption_reason"],
        "max_turn_duration"
    );
    assert_eq!(outcomes[0]["error"], Value::Null);
    stack.wait_phase(&session.id, "turn_completed").await;
    let uri = format!("/api/cards/{card_id}/harness/items");
    let (status, before) = stack.send("GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let terminal = before
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["method"] == "turn/completed")
        .unwrap();
    assert_eq!(
        terminal["turn_error_text"],
        "This turn exceeded its execution time limit and was interrupted."
    );
    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    let (status, after) = stack.send("GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        before, after,
        "a restart preserves the confirmed timeout and all transcript rows"
    );
    stack.shutdown().await;
}
