//! Grok Build as a managed ACP Planner and Assistant, through the production boot, REST routes,
//! Harness and MCP shim. The spy peer answers as Grok Build 1.0.62 does: no `agentInfo`, its
//! version at `_meta.agentVersion`, `cached_token` authentication, `_x.ai/*` notifications around
//! setup and turns, and a permission request before every MCP tool it calls.
use super::*;
use axum::http::Request;
use calm_server::acp_planner::test_seams::observe_held;

fn grok() -> Value {
    json!({"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VERSION":"1.0.62",
        "ACP_FIXTURE_AUTH_METHODS":"cached_token,grok.com","ACP_FIXTURE_VENDOR":"xai"})
}
async fn boot_grok(root: &Root) -> Stack {
    boot_as(root, "grok", grok(), "1.0.62").await
}
async fn create_grok(stack: &Stack) -> (String, String) {
    stack
        .create_claude_track_with(json!({"planner_provider":"grok","title":"Managed Grok"}))
        .await
}
fn lines(root: &Root, file: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join(file))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
/// The methods each agent process that served a prompt received, in order.
fn prompt_launches(root: &Root) -> Vec<Vec<String>> {
    let frames = lines(root, "requests.jsonl");
    frames
        .iter()
        .filter(|frame| frame["method"] == "session/prompt")
        .map(|prompt| {
            frames
                .iter()
                .filter(|frame| frame["_pid"] == prompt["_pid"])
                .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
                .collect()
        })
        .collect()
}
/// The argv of every Planner (not readiness or catalog) launch.
fn turn_argv(root: &Root) -> Vec<Value> {
    lines(root, "argv.jsonl")
        .into_iter()
        .zip(lines(root, "environment.jsonl"))
        .filter(|(_, launch)| launch["readiness"] == false && launch["catalog"] == false)
        .map(|(argv, _)| argv)
        .collect()
}
/// The key each session setup registered Neige's MCP server under.
fn server_keys(root: &Root) -> Vec<String> {
    lines(root, "requests.jsonl")
        .into_iter()
        .filter(|frame| frame["method"] == "session/new" || frame["method"] == "session/load")
        .map(|frame| {
            frame["params"]["mcpServers"][0]["name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}
async fn set_mode(stack: &Stack, card: &str, mode: &str) {
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/permission-mode"),
            Some(json!({ "permission_mode": mode })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A Grok Planner authenticates every launch before its session, runs two turns on one native
/// session, and loads that same session after a restart without resubmitting; Grok's `_x.ai/*`
/// notifications change nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_runs_two_planner_turns_and_loads_the_same_native_session_after_restart() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (status, providers) = stack.send("GET", "/api/agent-providers", None).await;
    assert_eq!(status, StatusCode::OK);
    let grok = providers
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "grok")
        .unwrap();
    assert_eq!(grok["status"], "ready", "{grok}");
    let (status, managed) = stack
        .send("GET", "/api/agent-providers/managed", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let capabilities = &managed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["availability"]["provider"] == "grok")
        .unwrap()["capabilities"];
    assert_eq!(
        capabilities,
        &json!({"managed_conversation":true,"steer":false,"images":false,"manual_compact":false,
            "history_replacement":false,"requires_ready_availability":true})
    );
    let (_, card) = create_grok(&stack).await;
    assert_eq!(
        stack.runtime(&card).await.agent_provider,
        Some(calm_server::session_projection_repo::AgentProvider::Grok)
    );
    for (n, text) in ["first grok input", "second grok input"].iter().enumerate() {
        let outcome = turn(&stack, &card, text, n + 1).await;
        assert_eq!(outcome["status"], "completed", "{outcome}");
    }
    let native = stack
        .runtime(&card)
        .await
        .session_id
        .expect("native binding");
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter().any(|row| row["item"]["text"]
            .as_str()
            .is_some_and(|text| text.ends_with("second grok input"))),
        "{rows:?}"
    );
    stack.shutdown().await;
    let stack = boot_grok(&root).await;
    assert_eq!(
        stack.runtime(&card).await.session_id.as_deref(),
        Some(native.as_str())
    );
    assert_eq!(
        requests(&root, "session/prompt").len(),
        2,
        "boot submits nothing"
    );
    assert_eq!(
        turn(&stack, &card, "after restart", 3).await["status"],
        "completed"
    );
    assert!(
        requests(&root, "session/load")
            .iter()
            .all(|request| request["params"]["sessionId"] == native)
    );
    let authenticated = |setup: &str| {
        vec![
            "initialize".to_owned(),
            "authenticate".into(),
            setup.into(),
            "session/prompt".into(),
        ]
    };
    assert_eq!(
        prompt_launches(&root),
        [
            authenticated("session/new"),
            authenticated("session/load"),
            authenticated("session/load")
        ]
    );
    for request in requests(&root, "authenticate") {
        assert_eq!(request["params"], json!({"methodId":"cached_token"}));
    }
    // Each launch registers Neige's server under a key of its own: `neige-` and 12 hex digits.
    let keys = server_keys(&root);
    assert_eq!(keys.len(), 3, "{keys:?}");
    for key in &keys {
        let suffix = key.strip_prefix("neige-").expect(key);
        assert!(
            suffix.len() == 12 && suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "{key}"
        );
    }
    assert_eq!(
        keys.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "{keys:?}"
    );
    stack.shutdown().await;
}

/// Neige's MCP tools never stop a Grok turn. `full` runs Grok with `--always-approve`, so it calls
/// them without asking; under `never` and `ask` it asks, and a call of a tool Neige's server lists
/// the session, under this launch's key, is allowed once without asking the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_calls_neiges_mcp_tools_without_asking_under_every_mode() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (track, card) = create_grok(&stack).await;
    std::fs::write(root.path().join("scenario"), "grok-mcp").unwrap();
    let mut held = observe_held(&stack.runtime(&card).await.id);
    for (n, mode) in ["never", "ask", "full"].into_iter().enumerate() {
        set_mode(&stack, &card, mode).await;
        let outcome = turn(&stack, &card, "write the report", n + 1).await;
        assert_eq!(outcome["status"], "completed", "{mode}: {outcome}");
        let replies: Vec<Value> = serde_json::from_str(
            &std::fs::read_to_string(root.path().join("grok-mcp-result.json")).unwrap(),
        )
        .unwrap();
        for reply in replies {
            assert!(
                reply.get("error").is_none() && reply["result"]["isError"] != true,
                "{mode}: {reply}"
            );
        }
        std::fs::remove_file(root.path().join("grok-mcp-result.json")).unwrap();
    }
    assert_eq!(
        lines(&root, "permission-log.jsonl"),
        vec![json!({"event":"reply","outcome":{"outcome":"selected","optionId":"allow-once"}}); 2],
        "the `never` and `ask` turns asked"
    );
    while let Ok(note) = held.try_recv() {
        assert!(
            !matches!(
                note,
                calm_server::acp_planner::test_seams::HeldNote::Open { .. }
            ),
            "{note:?}"
        );
    }
    assert!(
        stack
            .repo()
            .events_for_track(&track, &["ask.requested"], None)
            .await
            .unwrap()
            .is_empty()
    );
    // The peer logs its argv after its own script path.
    let stdio = json!(["agent", "--no-leader", "stdio"]);
    let always_approve = json!(["agent", "--always-approve", "--no-leader", "stdio"]);
    assert_eq!(turn_argv(&root), [stdio.clone(), stdio, always_approve]);
    let (status, report) = stack
        .send("GET", &format!("/api/tracks/{track}/report"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert!(
        report.to_string().contains("Written through use_tool"),
        "{report}"
    );
    stack.shutdown().await;
}

/// Under `ask`, a request for a tool Neige's server does not list a Planner, though named under
/// this launch's key, is put to the person. Grok ends its turn on the person's reject, and the turn
/// fails with that reason rather than as interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_asks_the_person_for_an_unlisted_tool_and_a_reject_ends_the_turn_with_why() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (track, card) = create_grok(&stack).await;
    set_mode(&stack, &card, "ask").await;
    std::fs::write(root.path().join("scenario"), "grok-mcp-unlisted").unwrap();
    let (status, body) = stack.post_input(&card, "accept the task").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let asked = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = stack
                .repo()
                .events_for_track(&track, &["ask.requested"], None)
                .await
                .unwrap();
            if let Some(row) = rows.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the request was put to the person");
    let key = server_keys(&root).pop().unwrap();
    let calm_server::event::Event::AskRequested { questions, .. } = &asked.event else {
        panic!("{asked:?}");
    };
    assert_eq!(questions[0].title, format!("{key}__neige_task_done"));
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/asks/{}/answer", asked.id),
            Some(json!({"answers": [{"option": 2}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let outcome = stack.wait_outcomes(&card, 1).await[0].clone();
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert_eq!(
        outcome["error"]["message"],
        format!("The agent ended the turn because the person declined `{key}__neige_task_done`."),
        "{outcome}"
    );
    assert_eq!(
        lines(&root, "permission-log.jsonl"),
        [json!({"event":"reply","outcome":{"outcome":"selected","optionId":"reject-once"}})]
    );
    stack.shutdown().await;
}

/// Under `never` a request for anything but Neige's own tools is answered `cancelled`; Grok ends its
/// turn, which fails naming the request, the mode and the modes that would let it through, rather
/// than as interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_other_permission_requests_keep_the_never_answer_and_say_why_the_turn_ended() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (_, card) = create_grok(&stack).await;
    std::fs::write(root.path().join("scenario"), "grok-shell").unwrap();
    let outcome = turn(&stack, &card, "run a command", 1).await;
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert_eq!(
        outcome["error"]["message"],
        "The agent ended the turn because Neige declined `Execute `echo one > one.txt`` under \
         the `never` permission mode. Switch the permission mode to `ask` to approve such a \
         request, or to `full` to allow it.",
        "{outcome}"
    );
    assert_eq!(
        lines(&root, "permission-log.jsonl"),
        [json!({"event":"reply","outcome":{"outcome":"cancelled"}})]
    );
    stack.shutdown().await;
}

/// A Grok Assistant is a managed, resumable Assistant session with only Assistant authority over
/// MCP, and runs its first turn through Grok.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grok_runs_an_assistant_turn_with_assistant_authority() {
    let root = Root::new("unused");
    let stack = boot_grok(&root).await;
    let (track, _) = create_grok(&stack).await;
    std::fs::write(root.path().join("scenario"), "assistant-mcp").unwrap();
    let (status, row) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", "grok-assistant"),
            Some(json!({"provider":"grok","text":"hello Grok assistant"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["provider"], "grok");
    let card = row["id"].as_str().unwrap();
    let outcome = stack.wait_outcomes(card, 1).await[0].clone();
    assert_eq!(outcome["status"], "completed", "{outcome}");
    let runtime = stack.runtime(card).await;
    assert_eq!(
        runtime.kind,
        calm_types::runtime::WorkerSessionKind::ManagedAssistant
    );
    let live = stack
        .repo()
        .session_get(&calm_types::worker::WorkerSessionId(runtime.id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (live.provider, live.contract, live.mode),
        (
            calm_types::worker::WorkerProviderKind::Grok,
            calm_types::worker::WorkerContract::Assistant,
            calm_types::worker::SessionMode::Resumable
        )
    );
    let mcp: Vec<Value> = serde_json::from_slice(
        &std::fs::read(root.path().join("assistant-mcp-results.json")).unwrap(),
    )
    .unwrap();
    for result in &mcp[..2] {
        assert!(result.get("error").is_none(), "{result}");
    }
    for result in &mcp[2..] {
        assert_eq!(result["error"]["code"], -32403, "{result}");
    }
    let prompt = requests(&root, "session/prompt");
    assert!(
        prompt[0].to_string().contains("What you cannot do"),
        "the Assistant instructions open its first turn"
    );
    stack.shutdown().await;
}
