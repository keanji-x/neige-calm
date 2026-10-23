//! Managed providers through the production POST, installed fake CLI and real harness.
#[allow(dead_code)]
#[path = "claude_planner_session_fixture.rs"]
mod claude_planner_session_fixture;
#[allow(dead_code)]
#[path = "claude_planner_stack_fixture.rs"]
mod stack_fixture;
use axum::http::{Request, StatusCode};
use calm_types::worker::{WorkerContract, WorkerSessionId};
use serde_json::{Value, json};
use stack_fixture::{Root, Stack};

async fn create(stack: &Stack, track: &str, key: &str, body: Value) -> (StatusCode, Value) {
    stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", key),
            Some(body),
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_provider_claude_first_turn_model_prompt_and_restart() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track, planner) = stack.create_claude_track().await;
    let root_before = stack.runtime(&planner).await.id;
    let body = json!({"provider":"claude", "text":"Assistant first turn", "model":"claude-fable-5-1[1m]", "reasoning_effort":"high"});
    let (status, row) = create(&stack, &track, "claude", body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["provider"], "claude");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    let runtime = stack.runtime(card).await;
    assert_eq!(
        runtime.kind,
        calm_types::runtime::WorkerSessionKind::ManagedAssistant
    );
    let live = stack
        .repo()
        .session_get(&WorkerSessionId(runtime.id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.contract, WorkerContract::Assistant);
    assert_ne!(runtime.id, root_before);
    let prompt = root.read_fake("instructions").unwrap();
    assert!(prompt.contains("What you cannot do"), "{prompt}");
    assert!(!prompt.contains("## Bound Template Input"));
    assert!(!prompt.contains("Codex may defer"));
    assert!(prompt.contains("mcp__neige__"));
    let argv = root.read_fake("argv").unwrap();
    assert!(argv.contains("--model=claude-fable-5-1[1m]"), "{argv}");
    assert!(argv.contains("--effort=high"), "{argv}");
    let token = root.spawned_token();
    assert!(stack.token_authenticates(&token).await);
    let (status, same) = create(&stack, &track, "claude", body).await;
    assert_eq!(status, StatusCode::CREATED, "{same}");
    assert_eq!(same["id"], card);
    let (status, response) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/permission-mode"),
            Some(json!({"permission_mode":"full"})),
        )
        .await;
    assert!(!status.is_success(), "{response}");
    let native = stack.runtime(card).await.session_id;
    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    assert!(!stack.token_authenticates(&token).await);
    assert_eq!(stack.runtime(card).await.session_id, native);
    let (status, response) = stack.post_input(card, "after restart").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stack.wait_outcomes(card, 2).await;
    assert_ne!(root.spawned_token(), token);
    managed_lifecycle(&stack, card, &track).await;
    let root_after: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(&track)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(root_after.as_deref(), Some(root_before.as_str()));
    stack.shutdown().await;
}

async fn boot_acp(root: &Root) -> Stack {
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let path = root.path().join("acp.json");
    std::fs::write(
        &path,
        json!({"agents":[{"provider":"opencode","command":"/usr/bin/python3",
        "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
        "env":{"ACP_FIXTURE_ROOT":root.path()},"min_agent_version":"1.0.0","default_permission_mode":"full"}]})
        .to_string(),
    )
    .unwrap();
    let mut config = root.config(false);
    config.acp_planner_config = Some(path);
    let exe = std::env::current_exe().unwrap();
    config.mcp_stdio_shim_bin = Some(
        exe.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("neige-mcp-stdio-shim"),
    );
    assert!(
        config.mcp_stdio_shim_bin.as_ref().unwrap().is_file(),
        "build neige-mcp-stdio-shim before the installed fake backend test"
    );
    Stack::boot_config(&config).await
}
fn requests(root: &Root, method: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["method"] == method)
        .collect()
}
async fn acp_assistant_frame(
    stack: &Stack,
    card: &str,
) -> calm_types::harness::snapshot::CanonicalHarnessSnapshot {
    serde_json::from_value(
        stack
            .runtime(card)
            .await
            .handle_state_json
            .expect("persisted canonical carrier"),
    )
    .unwrap()
}

async fn wait_acp_assistant_idle(stack: &Stack, card: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !acp_assistant_frame(stack, card).await.idle_for_lifecycle() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("original canonical ACP driver must reach END");
}

async fn acp_assistant_lifecycle(stack: &Stack, card: &str) {
    wait_acp_assistant_idle(stack, card).await;
    let before = stack.runtime(card).await;
    let count = stack.outcomes(card).await.len();
    stack
        .state
        .harness
        .remove(&before.id)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let (status, response) = stack.post_input(card, "lazy recovery").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stack.wait_outcomes(card, count + 1).await;
    wait_acp_assistant_idle(stack, card).await;
    assert_eq!(stack.runtime(card).await.id, before.id);
    assert_eq!(stack.runtime(card).await.session_id, before.session_id);
    let (status, response) = stack.reset(card).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        stack.runtime(card).await.id,
        before.id,
        "historical reset cannot replace a canonical carrier"
    );
    let (status, response) = stack.post_input(card, "after refused reset").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stack.wait_outcomes(card, count + 2).await;
    wait_acp_assistant_idle(stack, card).await;
    assert_eq!(stack.runtime(card).await.id, before.id);
    assert_eq!(stack.runtime(card).await.session_id, before.session_id);
    assert!(
        stack.row_hash(&before.id).await.is_none(),
        "ACP revokes after its exact driver exits"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_assistant_first_turn_model_prompt_and_restart() {
    let root = Root::new("unused");
    let stack = boot_acp(&root).await;
    let (track, planner) = stack
        .create_claude_track_with(json!({"planner_provider":"opencode"}))
        .await;
    let root_before = stack.runtime(&planner).await.id;
    std::fs::write(root.path().join("scenario"), "assistant-mcp").unwrap();
    let (status, row) = create(&stack, &track, "acp", json!({"provider":"opencode","text":"hello ACP assistant","model":"fixture/model-b","reasoning_effort":"deep"})).await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    let runtime = stack.runtime(card).await;
    assert_eq!(
        runtime.kind,
        calm_types::runtime::WorkerSessionKind::ManagedAssistant
    );
    let row = stack
        .repo()
        .session_get(&WorkerSessionId(runtime.id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.contract, WorkerContract::Assistant);
    let actor = calm_server::mcp_server::registry::ToolCallIdentity {
        card_id: card.into(),
        role: calm_server::model::CardRole::Assistant,
        provider: calm_types::runtime::AgentProvider::OpenCode,
        session_id: runtime.id.clone(),
        track_id: Some(track.clone()),
        area_id: "unused".into(),
        thread_id: "unused".into(),
    }
    .to_actor_id();
    assert!(
        matches!(actor, calm_types::ids::ActorId::AiManagedSession(_)),
        "{actor:?}"
    );
    assert_ne!(runtime.id, root_before);
    let native = runtime.session_id;
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
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 1);
    let text = prompts[0].to_string();
    assert!(text.contains("What you cannot do"), "{text}");
    assert!(!text.contains("Codex may defer"));
    let changes = requests(&root, "session/set_config_option");
    assert!(
        changes
            .iter()
            .any(|row| row["params"]["value"] == "fixture/model-b"),
        "{changes:?}"
    );
    assert!(
        changes.iter().any(|row| row["params"]["value"] == "deep"),
        "{changes:?}"
    );
    stack.shutdown().await;
    let stack = boot_acp(&root).await;
    assert_eq!(stack.runtime(card).await.session_id, native);
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    let (status, response) = stack.post_input(card, "after reboot").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stack.wait_outcomes(card, 2).await;
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    std::fs::write(root.path().join("scenario"), "permission").unwrap();
    let (status, response) = stack.post_input(card, "permission remains Never").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let outcomes = stack.wait_outcomes(card, 3).await;
    assert_eq!(outcomes[2]["status"], "interrupted", "{outcomes:?}");
    let permission: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("permission-reply.json")).unwrap())
            .unwrap();
    assert_eq!(permission["result"]["outcome"]["outcome"], "cancelled");
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    acp_assistant_lifecycle(&stack, card).await;
    let root_after: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(&track)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(root_after.as_deref(), Some(root_before.as_str()));
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_assistant_unknown_receipt_is_not_resent_after_boot() {
    use std::time::Duration;
    let root = Root::new("unused");
    let stack = boot_acp(&root).await;
    let (track, _) = stack
        .create_claude_track_with(json!({"planner_provider":"opencode"}))
        .await;
    let (status, row) = create(
        &stack,
        &track,
        "unknown",
        json!({"provider":"opencode","text":"initial"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    std::fs::write(root.path().join("scenario"), "lost").unwrap();
    let (status, response) = stack.post_input(card, "perform once").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let outcomes = stack.wait_outcomes(card, 2).await;
    assert!(
        outcomes[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown"),
        "{outcomes:?}"
    );
    let worker = stack.runtime(card).await.id;
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(
            stack.repo().sqlite_db().unwrap().read_pool(),
            &worker
        )
        .await
        .unwrap()
    );
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    let native = stack.runtime(card).await.session_id;
    let run_uri = format!("/api/cards/{card}/planner/run?contract=conversation_v1");
    let (status, run) = stack.send("GET", &run_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["input_contract"], "canonical");
    assert_eq!(
        run["recovery"],
        Value::Null,
        "no historical restart affordance: {run}"
    );
    assert_eq!(run["history_replacement_supported"], false);
    let before = acp_assistant_frame(&stack, card).await;
    assert!(matches!(
        before.canonical_inputs.attempts.last().unwrap().delivery,
        calm_types::conversation::InputDeliveryFact::Unknown
    ));
    stack
        .state
        .harness
        .remove(&worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    stack.shutdown().await;
    let stack = boot_acp(&root).await;
    let (status, run) = stack.send("GET", &run_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["input_contract"], "canonical");
    assert_eq!(run["history_replacement_supported"], false);
    let (status, response) = stack.post_input(card, "queued after unknown").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        requests(&root, "session/prompt").len(),
        2,
        "Unknown receipt cannot authorize native resend"
    );
    let after = acp_assistant_frame(&stack, card).await;
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    assert_eq!(
        after.canonical_inputs.pending.len(),
        before.canonical_inputs.pending.len() + 1
    );
    let (status, response) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/restart"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(stack.runtime(card).await.id, worker);
    assert_eq!(stack.runtime(card).await.session_id, native);
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    let (status, history) = stack
        .send(
            "GET",
            &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert!(
        history.to_string().contains("perform once"),
        "original history is kept: {history}"
    );
    stack.shutdown().await;
}

/// Exercise the production lazy-send, reset, revoke and delete compensation owners.
async fn managed_lifecycle(stack: &Stack, card: &str, track: &str) {
    let before = stack.runtime(card).await;
    let outcomes = stack.outcomes(card).await.len();
    let handle = stack.state.harness.remove(&before.id).unwrap();
    handle.shutdown().await.unwrap();
    let (status, response) = stack.post_input(card, "lazy recovery").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stack.wait_outcomes(card, outcomes + 1).await;
    assert_eq!(
        stack.runtime(card).await.id,
        before.id,
        "lazy send preserves native history"
    );
    let (status, response) = stack.reset(card).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let reset = stack.runtime(card).await;
    assert_ne!(reset.id, before.id);
    assert!(
        stack.row_hash(&before.id).await.is_none(),
        "superseded managed credentials are revoked"
    );
    assert!(
        stack.row_hash(&reset.id).await.is_none(),
        "a replacement cannot inherit its predecessor's hash"
    );
    let (status, response) = stack.post_input(card, "new native history").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    wait_runtime_outcomes(stack, &reset.id, 1).await;
    assert_eq!(
        stack.row_hash(&reset.id).await.is_some(),
        reset.agent_provider == Some(calm_types::runtime::AgentProvider::Claude),
        "ACP revokes when its process exits; Claude retains its session credential"
    );
    sqlx::query(
        "CREATE TRIGGER assistant_block_delete BEFORE DELETE ON tracks
         BEGIN SELECT RAISE(FAIL, 'delete blocked'); END",
    )
    .execute(stack.repo().sqlite_db().unwrap().writer_pool())
    .await
    .unwrap();
    let (status, response) = stack
        .send("DELETE", &format!("/api/tracks/{track}"), None)
        .await;
    sqlx::query("DROP TRIGGER assistant_block_delete")
        .execute(stack.repo().sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    assert!(!status.is_success(), "{response}");
    assert!(stack.repo().track_get(track).await.unwrap().is_some());
    assert!(
        stack.state.harness.get(&reset.id).is_some(),
        "aborted deletion must reinstall the Assistant"
    );
    assert!(
        stack.row_hash(&reset.id).await.is_none(),
        "aborted deletion still revokes credentials"
    );
    let (status, response) = stack.post_input(card, "after aborted deletion").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    wait_runtime_outcomes(stack, &reset.id, 2).await;
    assert_eq!(
        stack.row_hash(&reset.id).await.is_some(),
        reset.agent_provider == Some(calm_types::runtime::AgentProvider::Claude),
        "the next turn owns its provider's credential lifetime"
    );
}

async fn wait_runtime_outcomes(stack: &Stack, worker: &str, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let session = stack
                .repo()
                .session_get(&WorkerSessionId(worker.into()))
                .await
                .unwrap()
                .unwrap();
            let rows = match (session.card_id.as_ref(), session.thread_id.as_ref()) {
                (Some(card), Some(thread)) => stack
                    .repo()
                    .transcript_rows_of_thread(card.as_str(), thread)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|row| row.method == "turn/completed")
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            if rows.len() >= count {
                let outcome: Value = serde_json::from_str(&rows[count - 1].params).unwrap();
                assert_eq!(outcome["status"], "completed", "{outcome}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("this runtime's own turn must complete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_provider_shutdown_replay_preserves_each_provider_cleanup_contract() {
    use calm_types::runtime::{AgentProtocol, AgentProvider};
    use std::os::unix::process::CommandExt;
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let root = Root::new("exit");
        let stack = if provider == AgentProvider::Claude {
            Stack::boot(&root).await
        } else {
            boot_acp(&root).await
        };
        let (track, _) = stack
            .create_claude_track_with(json!({"planner_provider":provider}))
            .await;
        let (status, row) = create(
            &stack,
            &track,
            "orphan",
            json!({"provider":provider,"text":"start native Assistant"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{row}");
        let card = row["id"].as_str().unwrap();
        stack.wait_outcomes(card, 1).await;
        let runtime = stack.runtime(card).await;
        stack
            .state
            .harness
            .remove(&runtime.id)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        let wiring = stack.state.planner_wiring();
        let (key, marker) = match provider.protocol() {
            AgentProtocol::Claude => (
                calm_server::claude_planner::stop::MARKER_KEY,
                wiring.claude.instance.marker(&runtime.id),
            ),
            AgentProtocol::Acp => (
                calm_server::acp_planner::config::MARKER_KEY,
                wiring.acp.instance.marker(&runtime.id),
            ),
            AgentProtocol::Codex => unreachable!(),
        };
        let mut command = tokio::process::Command::new("/bin/sleep");
        command
            .arg("300")
            .env_clear()
            .env(key, &marker)
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let expected = format!("{key}={marker}");
            loop {
                if std::fs::read(format!("/proc/{pid}/environ"))
                    .unwrap_or_default()
                    .split(|b| *b == 0)
                    .any(|entry| entry == expected.as_bytes())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // ACP cleanup must work without a native thread and must not reopen a session.
        if provider == AgentProvider::OpenCode {
            sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?1")
                .bind(&runtime.id)
                .execute(stack.repo().sqlite_db().unwrap().writer_pool())
                .await
                .unwrap();
        }
        let native_before = ["session/new", "session/load", "session/prompt"]
            .map(|method| requests(&root, method).len());
        let submitted = stack
            .state
            .operation_runtime
            .submit(
                "planner-harness-shutdown",
                calm_server::operation::OperationKey {
                    operation_key: calm_server::model::new_id(),
                    idempotency_key: None,
                    payload_hash: calm_server::model::new_id(),
                },
                serde_json::to_value(calm_server::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload {
                    worker_session_id: runtime.id,
                }).unwrap(),
            )
            .await;
        let outcome = match submitted {
            Ok(id) => stack.state.operation_runtime.wait(&id).await,
            Err(error) => Err(error),
        };
        let stopped = child.try_wait().unwrap().is_some();
        if !stopped {
            child.kill().await.unwrap();
        }
        child.wait().await.unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(
            stopped,
            provider == AgentProvider::OpenCode,
            "Claude runtime cleanup uses Engine ownership; ACP retains its native marker until C4.3"
        );
        assert_eq!(
            ["session/new", "session/load", "session/prompt"]
                .map(|method| requests(&root, method).len()),
            native_before,
            "orphan cleanup must not reopen or prompt"
        );
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_assistant_images_false_refuses_run_and_upload() {
    images_false_refuses(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_assistant_images_false_refuses_before_binding_or_queue() {
    images_false_refuses(false).await;
}

async fn images_false_refuses(check_upload: bool) {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let root = Root::new("unused");
    let stack = boot_acp(&root).await;
    let (track, _) = stack
        .create_claude_track_with(json!({"planner_provider":"opencode"}))
        .await;
    let (status, row) = create(
        &stack,
        &track,
        "images",
        json!({"provider":"opencode","text":"hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    let pool = stack.repo().sqlite_db().unwrap();
    let workspace: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id=?")
        .bind(&track)
        .fetch_one(pool.read_pool())
        .await
        .unwrap();
    let attachment_root = std::path::Path::new(&workspace).join(".neige/attachments");
    let id = "12345678-1234-4234-8234-123456789abc.png";
    let staged = attachment_root.join(card).join("staging").join(id);
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, b"\x89PNG\r\n\x1a\nimage").unwrap();
    // An outcome is durable before the run loop finishes its VCS/phase write.
    // Join that real loop before comparing the complete persistence snapshot.
    let session = stack.runtime(card).await;
    stack
        .state
        .harness
        .get(&session.id)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let before = stack.runtime(card).await;
    if check_upload {
        let (_, run) = stack
            .send(
                "GET",
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
                None,
            )
            .await;
        assert_eq!(run["attachments_supported"], false, "{run}");
        let response = stack
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card}/planner/attachments"))
                    .header("x-calm-actor", "user")
                    .body(Body::from(b"\x89PNG\r\n\x1a\nimage".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let error = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error:?}");
        assert!(String::from_utf8_lossy(&error).contains("images"));
        stack.shutdown().await;
        return;
    }
    let (status, error) = stack
        .send_input(card, json!({"text":"image","attachments":[id]}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(
        error["error"].as_str().unwrap().contains("images"),
        "{error}"
    );
    assert!(staged.is_file(), "refusal must leave staging sweepable");
    assert!(!attachment_root.join(card).join("bound").join(id).exists());
    let after = stack.runtime(card).await;
    assert_eq!(after.id, before.id);
    assert_eq!(
        after.handle_state_json, before.handle_state_json,
        "no queue or issuance side effect"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_assistant_archive_survives_cold_boot_and_restores_the_same_native_context() {
    let root = Root::new("exit");
    let stack = boot_acp(&root).await;
    let (track, _) = stack
        .create_claude_track_with(json!({"planner_provider":"opencode"}))
        .await;
    let (status, row) = create(
        &stack,
        &track,
        "archive-cold-boot",
        json!({"provider":"opencode","text":"saved context"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    let original = stack.runtime(card).await;
    wait_acp_assistant_idle(&stack, card).await;
    let path = format!("/api/tracks/{track}/conversations/{card}/archive");
    assert_eq!(
        stack
            .send("PATCH", &path, Some(json!({"archived":true})))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let history = stack
        .send(
            "GET",
            &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
            None,
        )
        .await
        .1;
    let prompts = requests(&root, "session/prompt").len();
    let loads = requests(&root, "session/load").len();
    // This tears down the old AppState, MCP listener and every live harness before production boot.
    stack.shutdown().await;
    let stack = boot_acp(&root).await;
    assert!(stack.state.harness.get(&original.id).is_none());
    assert_eq!(requests(&root, "session/prompt").len(), prompts);
    assert_eq!(requests(&root, "session/load").len(), loads);
    let (_, listed) = stack
        .send("GET", &format!("/api/tracks/{track}/conversations"), None)
        .await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == card)
            .unwrap()["archivedAt"]
            .is_number()
    );
    assert_eq!(
        stack.post_input(card, "refused while archived").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        stack
            .send(
                "GET",
                &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
                None
            )
            .await
            .1,
        history
    );
    assert_eq!(
        stack
            .send("PATCH", &path, Some(json!({"archived":false})))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let restored = stack.runtime(card).await;
    assert_eq!(restored.id, original.id);
    assert_eq!(restored.session_id, original.session_id);
    assert_eq!(restored.thread_id, original.thread_id);
    let (status, body) = stack.post_input(card, "continue saved context").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.wait_outcomes(card, 2).await;
    assert_eq!(stack.runtime(card).await.session_id, original.session_id);
    assert_eq!(requests(&root, "session/prompt").len(), prompts + 1);
    stack.shutdown().await;
}
