#[path = "cases/acp_antigravity_compact.rs"]
mod acp_antigravity_compact;
#[path = "cases/acp_antigravity_mcp.rs"]
mod acp_antigravity_mcp;
#[path = "cases/acp_canonical.rs"]
mod acp_canonical;
#[path = "cases/acp_compact.rs"]
mod acp_compact;
#[path = "cases/acp_grok_compact.rs"]
mod acp_grok_compact;
#[path = "cases/acp_images.rs"]
mod acp_images;
// Managed ACP acceptance through the production boot, REST routes and Harness.
#[path = "cases/acp_grok_provider.rs"]
mod acp_grok_provider;
#[path = "cases/acp_planner_approvals.rs"]
mod acp_planner_approvals;
#[path = "cases/acp_planner_authentication.rs"]
mod acp_planner_authentication;
#[path = "cases/acp_planner_catalog.rs"]
mod acp_planner_catalog;
#[path = "cases/acp_planner_checkpoint.rs"]
mod acp_planner_checkpoint;
#[path = "cases/acp_planner_cli.rs"]
mod acp_planner_cli;
#[path = "cases/acp_planner_lifecycle.rs"]
mod acp_planner_lifecycle;
#[path = "cases/acp_planner_permission_creation.rs"]
mod acp_planner_permission_creation;
#[path = "cases/acp_planner_prompt_error.rs"]
mod acp_planner_prompt_error;
#[path = "cases/acp_planner_start.rs"]
mod acp_planner_start;
#[path = "cases/acp_planner_transcript.rs"]
mod acp_planner_transcript;
#[path = "cases/acp_settlement.rs"]
mod acp_settlement;
#[path = "cases/c4_acp_execution.rs"]
mod c4_acp_execution;
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
    boot_with(root, json!({}), "1.0.0").await
}
/// Boot with `env` added to the fixture's registered environment and `min_agent_version`.
async fn boot_with(root: &Root, env: Value, min_agent_version: &str) -> Stack {
    boot_as(root, "opencode", env, min_agent_version).await
}
/// Boot with the fixture registered as `provider`.
async fn boot_as(root: &Root, provider: &str, env: Value, min_agent_version: &str) -> Stack {
    boot_as_with_creation_mode(root, provider, env, min_agent_version, None).await
}
async fn boot_as_with_creation_mode(
    root: &Root,
    provider: &str,
    env: Value,
    min_agent_version: &str,
    mode: Option<&str>,
) -> Stack {
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let config_path = root.path().join("acp.json");
    let mut env = env;
    env["ACP_FIXTURE_ROOT"] = json!(root.path());
    let mut agent = json!({"provider":provider,"command":"/usr/bin/python3","args":["-u",PEER],"env":env,"min_agent_version":min_agent_version});
    if let Some(mode) = mode {
        agent["default_permission_mode"] = json!(mode);
    }
    std::fs::write(&config_path, json!({"agents":[agent]}).to_string()).unwrap();
    let mut config = root.config(false);
    config.acp_planner_config = Some(config_path);
    // Use this build's production shim, never a host installation found on PATH.
    let executable = std::env::current_exe().unwrap();
    let shim = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("neige-mcp-stdio-shim");
    assert!(
        shim.is_file(),
        "build neige-mcp-stdio-shim before running the ACP stack tests"
    );
    config.mcp_stdio_shim_bin = Some(shim);
    Stack::boot_config(&config).await
}
async fn create(stack: &Stack) -> (String, String) {
    stack
        .create_historical_track_with(json!({"planner_provider":"opencode","title":"Managed ACP"}))
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
    wait_file_within(root, name, Duration::from_secs(20)).await;
}
async fn wait_file_within(root: &Root, name: &str, budget: Duration) {
    tokio::time::timeout(budget, async {
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
        rows.iter().any(|row| row["item"]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("reply: ")
                && text.ends_with("User says:\nfirst explicit input"))),
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
    assert_eq!(requests(&root, "session/resume").len(), 2);
    assert!(
        requests(&root, "session/resume")
            .iter()
            .all(|request| request["params"]["sessionId"] == native)
    );
    for method in ["session/new", "session/load", "session/resume"] {
        for request in requests(&root, method) {
            assert_eq!(
                request["params"]["mcpServers"][0]["args"],
                json!(["--structured-content-as-text"]),
                "every native MCP setup must expose the complete tool result"
            );
        }
    }
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
    let mut held =
        calm_server::acp_planner::test_seams::observe_held(&stack.runtime(&card).await.id);
    assert_eq!(
        turn(&stack, &card, "request permission", 1).await["status"],
        "interrupted"
    );
    assert!(
        held.try_recv().is_err(),
        "a never turn reports nothing to the held-request channel"
    );
    assert_eq!(
        acp_planner_approvals::launch_permissions(&root),
        vec![json!({"permissions":[{"action":"question","resource":"*","effect":"deny"}]})],
        "a never turn denies native questions while retaining the operator's other permissions"
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
    let pool = stack.repo().sqlite_db().unwrap();
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(pool.read_pool(), &runtime.id)
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
        let operational = requests(&root, "session/prompt")
            .iter()
            .any(|request| request["_pid"] == presence["_pid"]);
        assert_eq!(presence["NEIGE_MCP_TOKEN"], operational);
        assert_eq!(presence["NEIGE_MCP_SOCKET"], operational);
        assert_eq!(presence["ACP_AMBIENT_SENTINEL"], false);
    }
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_registry_miss_recovers_without_codex_readiness() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "before registry miss", 1).await["status"],
        "completed"
    );
    let runtime = stack.runtime(&card).await;
    let handle = stack.state.harness.remove(&runtime.id).expect("registered");
    handle.shutdown().await.unwrap();
    assert_eq!(
        turn(&stack, &card, "after registry miss", 2).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_recovery_records_unknown_for_a_dispatched_turn_with_no_pending_queue() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let outcome = turn(&stack, &card, "dispatched checkpoint", 1).await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_db().unwrap();
    let mut snapshot = stack.harness(&runtime.id).snapshot().await.unwrap();
    stack.shutdown().await;
    // Restore the durable state at the real post-dispatch/pre-settlement crash checkpoint.
    snapshot.phase = calm_server::harness::HarnessPhaseTag::TurnRunning;
    assert!(snapshot.pending_entries().is_empty());
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                            let mut tx = write_session.begin_immediate().await.unwrap();
                            sqlx::query(
                    "UPDATE acp_submissions SET state='sending',outcome_json=NULL WHERE worker_session_id=?1",
                )
                .bind(&runtime.id)
                .execute(&mut **tx)
                .await
                .unwrap();
                            delete_checkpoint_rows(&mut tx, &card, "turn/completed").await;
                            sqlx::query(
                                "UPDATE worker_sessions SET handle_state_json=?2,active_turn_id=?3 WHERE id=?1",
                            )
                            .bind(&runtime.id)
                            .bind(serde_json::to_string(&snapshot).unwrap())
                            .bind(outcome["id"].as_str().unwrap())
                            .execute(&mut **tx)
                            .await
                            .unwrap();
                            tx.commit().await.unwrap();
                            let stack = boot(&root).await;
                            let outcomes = stack.wait_outcomes(&card, 1).await;
                            assert_eq!(outcomes[0]["id"], outcome["id"]);
                            assert_eq!(outcomes[0]["status"], "failed");
                            assert!(
                                outcomes[0]["error"]["message"]
                                    .as_str()
                                    .unwrap()
                                    .contains("unknown")
                            );
                            assert_eq!(
                                requests(&root, "session/prompt").len(),
                                1,
                                "recovery cannot resend"
                            );
                            assert!(
                                calm_server::db::sqlite::acp_submission_unresolved(
                                    pool.read_pool(),
                                    &runtime.id
                                )
                                .await
                                .unwrap()
                            );
                            stack.shutdown().await;
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_reset_after_dispatch_checkpoint_transfers_only_later_input() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "checkpoint").unwrap();
    let (status, body) = stack.post_input(&card, "original operational input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "setup-checkpoint").await;
    let runtime = stack.runtime(&card).await;
    let mut checkpoint = calm_server::harness::HarnessSnapshot::from_value_strict(
        runtime.handle_state_json.clone().unwrap(),
    );
    assert_eq!(
        checkpoint.phase,
        calm_server::harness::HarnessPhaseTag::IssuingTurn
    );
    assert!(!checkpoint.pending_entries().is_empty());
    std::fs::write(root.path().join("release-setup"), "").unwrap();
    stack.wait_outcomes(&card, 1).await;
    let pool = stack.repo().sqlite_db().unwrap();
    stack.shutdown().await;
    let mut entries = checkpoint.pending_entries();
    entries.push(calm_server::harness::QueueEntry::user_message(
        "later input".into(),
        None,
        Vec::new(),
    ));
    checkpoint.set_pending_entries(entries);
    (pool)
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
                .bind(&runtime.id)
                .bind(serde_json::to_string(&checkpoint).unwrap()),
        )
        .await
        .unwrap();
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    // Boot can immediately issue the later input. Hold its real drain seam so the
    // queue-ownership assertions and reset observe the intended pre-dispatch state.
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    calm_server::harness::run_loop::install_planner_harness_drain_race_hook_for_test(
        calm_server::harness::run_loop::ANY_RUNTIME,
        calm_server::harness::run_loop::PlannerHarnessDrainRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let stack = boot(&root).await;
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("recovered later input reaches the drain seam");
    let queued = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .unwrap()
        .pending_entries();
    assert!(queued.iter().all(|entry|!matches!(entry.observation(),calm_server::harness::Observation::UserMessage{text} if text=="original operational input")));
    assert!(queued.iter().any(|entry|matches!(entry.observation(),calm_server::harness::Observation::UserMessage{text} if text=="later input")));
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/reset"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    release.notify_one();
    // Reset clears the old transcript; this is the later input's own terminal outcome.
    stack.wait_outcomes(&card, 1).await;
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 2);
    let last = prompts.last().unwrap()["params"]["prompt"]
        .as_array()
        .unwrap();
    assert!(last.iter().any(|part| {
        part["text"]
            .as_str()
            .is_some_and(|text| text.contains("later input"))
    }));
    assert!(last.iter().all(|part| {
        part["text"]
            .as_str()
            .is_none_or(|text| !text.contains("original operational input"))
    }));
    assert!(stack.repo().track_get(&track).await.unwrap().is_some());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_failed_cleanup_revokes_the_live_mcp_credential() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "hold").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let pool = stack.repo().sqlite_db().unwrap();
    let before: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(
        before.is_some(),
        "exercise a minted credential, not an unconfigured path"
    );
    stack.block_provider_stop().await;
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    let outcomes = stack.wait_outcomes(&card, 1).await;
    stack.clear_provider_fault().await;
    assert_eq!(outcomes[0]["status"], "failed");
    let after: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(
        after.is_none(),
        "failed process cleanup must not retain MCP authority"
    );
    stack.shutdown().await;
}

async fn delete_checkpoint_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    method: &str,
) {
    sqlx::query("DELETE FROM harness_items WHERE card_id=?1 AND method=?2")
        .bind(card)
        .bind(method)
        .execute(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_completed_receipt_restores_reply_after_harness_projection_crash() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let outcome = turn(&stack, &card, "retain this reply", 1).await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_db().unwrap();
    let mut checkpoint = stack.harness(&runtime.id).snapshot().await.unwrap();
    stack.shutdown().await;
    checkpoint.phase = calm_server::harness::HarnessPhaseTag::TurnRunning;
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                delete_checkpoint_rows(&mut tx, &card, "item/completed").await;
                delete_checkpoint_rows(&mut tx, &card, "item/started").await;
                delete_checkpoint_rows(&mut tx, &card, "turn/completed").await;
                sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
                    .bind(&runtime.id)
                    .bind(serde_json::to_string(&checkpoint).unwrap())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let stack = boot(&root).await;
                let rows = stack
                    .repo()
                    .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
                    .await
                    .unwrap();
                assert!(
                    rows.iter()
                        .any(|row| row.params.contains("retain this reply")
                            && row.item_type.as_deref() == Some("agentMessage")),
                    "completed reply must recover from its receipt"
                );
                let completed: Vec<Value> = rows
                    .iter()
                    .filter(|row| row.method == "item/completed")
                    .map(|row| serde_json::from_str::<Value>(&row.params).unwrap()["item"].clone())
                    .collect();
                assert_eq!(completed.len(), 3);
                assert_eq!(completed[0]["text"], "before operation");
                assert_eq!(completed[1]["type"], "dynamicToolCall");
                assert!(
                    completed[2]["text"]
                        .as_str()
                        .unwrap()
                        .contains("retain this reply")
                );
                assert_eq!(
                    completed
                        .iter()
                        .map(|item| item["id"].as_str().unwrap())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len(),
                    3
                );
                assert_eq!(stack.outcomes(&card).await[0]["id"], outcome["id"]);
                assert_eq!(requests(&root, "session/prompt").len(), 1);
                stack.shutdown().await;
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_operation_replay_uses_the_quiesced_full_checkpoint() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let pool = stack.repo().sqlite_db().unwrap();
    let operation: String = sqlx::query_scalar("SELECT id FROM operations WHERE target_id=?1 AND phase='succeeded' ORDER BY created_at_ms DESC LIMIT 1")
        .bind(&card).fetch_one(pool.read_pool()).await.unwrap();
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "already dispatched").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let issued = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .unwrap()
        .last_turn_id
        .unwrap();
    // Crash replay uses the operation's original Idle snapshot while boot has
    // independently recovered and dispatched this runtime's current input.
    (pool).writer().execute(sqlx::query("UPDATE operations SET phase='spawn_started',phase_detail_json=NULL,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
        .bind(&operation)).await.unwrap();
    calm_server::recover_operations_on_boot(&stack.state)
        .await
        .unwrap();
    let restored = stack.harness(&runtime.id).snapshot().await.unwrap();
    assert_eq!(restored.last_turn_id.as_deref(), Some(issued.as_str()));
    assert!(restored.pending_entries().is_empty());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    assert_eq!(stack.wait_outcomes(&card, 1).await[0]["id"], issued);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_recovery_retires_dispatch_before_folding_later_report_edits() {
    use calm_server::event::{EditAuthor, Event, EventScope};
    use calm_server::harness::Observation;
    use calm_server::ids::{ActorId, CardId, TrackId};
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    std::fs::write(root.path().join("scenario"), "checkpoint").unwrap();
    stack
        .harness(&runtime.id)
        .observe(Observation::ReportEdited {
            presentation: Default::default(),
            track_id: TrackId::from(track.clone()),
            body_sha256: "original".into(),
            body: "original report".into(),
            author: Some(EditAuthor::User),
            body_before: None,
            doc_rev_after: None,
            blocks_after: None,
        })
        .unwrap();
    wait_file_within(&root, "setup-checkpoint", Duration::from_secs(50)).await;
    let checkpoint = calm_server::harness::HarnessSnapshot::from_value_strict(
        stack.runtime(&card).await.handle_state_json.unwrap(),
    );
    assert_eq!(
        checkpoint.phase,
        calm_server::harness::HarnessPhaseTag::IssuingTurn
    );
    assert!(matches!(
        checkpoint.pending_entries()[0].observation(),
        Observation::ReportEdited { .. }
    ));
    std::fs::write(root.path().join("release-setup"), "").unwrap();
    stack.wait_outcomes(&card, 1).await;
    let pool = stack.repo().sqlite_db().unwrap();
    stack.shutdown().await;
    let write_db = (pool).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate()
            .await
            .unwrap();
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
            .bind(&runtime.id)
            .bind(serde_json::to_string(&checkpoint).unwrap())
            .execute(&mut **tx)
            .await
            .unwrap();
        let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
            .bind(&track)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
        let report: String =
            sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND kind='track-report'")
                .bind(&track)
                .fetch_one(&mut **tx)
                .await
                .unwrap();
        let later = calm_server::db::sqlite::append_decision_event_in_tx(
            &mut tx,
            &ActorId::User,
            &EventScope::Track {
                area: area.into(),
                track: TrackId::from(track.clone()),
            },
            None,
            &Event::TrackReportEdited {
                track_id: track.clone().into(),
                card_id: CardId::from(report),
                author: EditAuthor::User,
                author_plugin_id: None,
                edit_id: "later-edit".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: "original report".into(),
                body_after: "later report".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let stack = boot(&root).await;
        let recovered = stack.harness(&runtime.id).snapshot().await.unwrap();
        let entries = recovered.pending_entries();
        assert_eq!(
            entries.len(),
            1,
            "only the undispatched report edit remains"
        );
        assert_eq!(entries[0].envelope_id(), Some(later));
        assert!(
            matches!(entries[0].observation(), Observation::ReportEdited { body, .. } if body == "later report")
        );
        assert_eq!(requests(&root, "session/prompt").len(), 1);
        stack.shutdown().await;

    })).await.expect("independent write operation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_reset_does_not_mirror_the_predecessors_revoked_hash() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "hold").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let pool = stack.repo().sqlite_db().unwrap();
    let old: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(old.is_some());
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            None,
        )
        .await;
    assert!(status.is_success(), "{body}");
    stack.wait_outcomes(&card, 1).await;
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/reset"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{body}");
    let current = stack.runtime(&card).await;
    assert_ne!(current.id, runtime.id);
    let mirrored: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&current.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(
        mirrored.is_none(),
        "idle successor cannot inherit an old credential"
    );
    let active:i64=sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE mcp_token_hash=?1 AND state IN ('starting','running','idle','turn_pending')").bind(old).fetch_one(pool.read_pool()).await.unwrap();
    assert_eq!(active, 0);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c4_consecutive_acp_turns_launch_with_distinct_execution_aliases() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    for n in 1..=2 {
        assert_eq!(
            turn(&stack, &card, "engine turn", n).await["status"],
            "completed"
        );
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT execution_id,intent_json FROM workspace_executions WHERE json_extract(intent_json,'$.consumer')='acp-planner'",
    ).fetch_all(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(rows.len(), 2, "each ACP turn must use the real Engine");
    let aliases = rows
        .iter()
        .map(|(_, json)| {
            serde_json::from_str::<workspace_runtime::execution::ExecutionMetadata>(json)
                .unwrap()
                .alias
                .0
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(aliases.len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_reuses_one_server_and_revokes_card_authority_on_reset() {
    let root = Root::new("unused");
    let stack = boot_as(
        &root,
        "antigravity",
        json!({
            "ACP_FIXTURE_AGENT_NAME": "Antigravity ACP Server",
            "ACP_FIXTURE_VERSION": "1.5.0",
            "ACP_FIXTURE_CAPTURE_SHARED_TOKEN": "1",
        }),
        "1.5.0",
    )
    .await;
    let (first_track, first) = stack
        .create_historical_track_with(json!({
            "planner_provider": "antigravity", "title": "First AGY card",
        }))
        .await;
    let (_, second) = stack
        .create_historical_track_with(json!({
            "planner_provider": "antigravity", "title": "Second AGY card",
        }))
        .await;

    assert_eq!(
        turn(&stack, &first, "first", 1).await["status"],
        "completed"
    );
    let first_native = stack.runtime(&first).await.session_id.unwrap();
    assert_eq!(
        turn(&stack, &first, "second", 2).await["status"],
        "completed"
    );
    assert_eq!(
        stack.runtime(&first).await.session_id.as_deref(),
        Some(first_native.as_str())
    );

    std::fs::write(root.path().join("scenario"), "shared-hold").unwrap();
    assert_eq!(
        stack.post_input(&first, "cancel this card").await.0,
        StatusCode::OK
    );
    assert_eq!(
        stack.post_input(&second, "leave this card running").await.0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        while requests(&root, "session/prompt").len() < 4 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    let second_native = stack.runtime(&second).await.session_id.unwrap();
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{first}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stack.wait_outcomes(&first, 3).await[2]["status"],
        "interrupted"
    );
    assert_eq!(
        stack.wait_outcomes(&second, 1).await[0]["status"],
        "completed"
    );

    // The reset endpoint must revoke the old session before returning success.
    let first_token =
        std::fs::read_to_string(root.path().join(format!("{first_native}.test-token"))).unwrap();
    let second_token =
        std::fs::read_to_string(root.path().join(format!("{second_native}.test-token"))).unwrap();
    assert!(stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    let stream = tokio::net::UnixStream::connect(stack.mcp_socket())
        .await
        .unwrap();
    let mut stream = tokio::io::BufReader::new(stream);
    let initialized = mcp_exchange(
        &mut stream,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "shared-acp-test", "version": "1"},
                "_meta": {"dev.neige/auth": {"token": first_token}}}
        }),
    )
    .await;
    assert!(initialized.get("result").is_some());
    let listed = mcp_exchange(
        &mut stream,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let tools = listed["result"]["tools"]
        .as_array()
        .expect("listed MCP tools");
    for (number, name, args) in [
        (10, "neige_track_status", json!({})),
        (11, "neige_cat", json!({"path": "guide/report.md"})),
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "{name} missing"
        );
        let response = mcp_exchange(
            &mut stream,
            json!({
                "jsonrpc": "2.0", "id": number, "method": "tools/call",
                "params": {"name": name, "arguments": args}
            }),
        )
        .await;
        assert!(
            response["result"].is_object() && response["result"]["isError"] != true,
            "{response}"
        );
    }
    let (status, body) = stack.reset(&first).await;
    assert!(status.is_success(), "{body}");
    assert!(!stack.token_authenticates(&first_token).await);
    assert!(stack.token_authenticates(&second_token).await);
    assert!(
        mcp_exchange(
            &mut stream,
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}
            })
        )
        .await
        .get("error")
        .is_some()
    );
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 4);
    assert!(
        prompts
            .iter()
            .all(|prompt| prompt["_pid"] == prompts[0]["_pid"])
    );
    assert_eq!(prompts[0]["params"]["sessionId"], first_native);
    assert_eq!(prompts[1]["params"]["sessionId"], first_native);
    assert!(
        prompts[2..]
            .iter()
            .any(|prompt| prompt["params"]["sessionId"] == second_native)
    );
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let (status, body) = stack
        .send_with(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{first_track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", uuid::Uuid::new_v4().to_string()),
            Some(json!({"provider": "antigravity", "text": "assistant reply"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let assistant = body["id"].as_str().unwrap();
    assert_eq!(
        stack.wait_outcomes(assistant, 1).await[0]["status"],
        "completed"
    );
    let runtime = stack.runtime(assistant).await;
    acp_compact::wait_phase(&stack, &runtime.id, "turn_completed").await;
    let native = runtime.session_id.unwrap();
    let token = std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap();
    assert!(stack.token_authenticates(&token).await);
    let assistant_stream = tokio::net::UnixStream::connect(stack.mcp_socket())
        .await
        .unwrap();
    let mut assistant_stream = tokio::io::BufReader::new(assistant_stream);
    assert!(
        mcp_exchange(
            &mut assistant_stream,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": {"name": "shared-acp-test", "version": "1"},
                    "_meta": {"dev.neige/auth": {"token": token}}}
            })
        )
        .await
        .get("result")
        .is_some()
    );

    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{first_track}/conversations/{assistant}/archive"),
            Some(json!({"archived": true})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!stack.token_authenticates(&token).await);
    assert!(
        mcp_exchange(
            &mut assistant_stream,
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
            })
        )
        .await
        .get("error")
        .is_some()
    );

    assert!(stack.token_authenticates(&second_token).await);
    stack.shutdown().await;
}

async fn mcp_exchange(
    stream: &mut tokio::io::BufReader<tokio::net::UnixStream>,
    frame: Value,
) -> Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    stream
        .get_mut()
        .write_all(format!("{frame}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
