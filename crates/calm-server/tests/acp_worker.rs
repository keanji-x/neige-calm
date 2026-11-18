//! Task ACP acceptance through the production boot, declaration, scheduler, MCP and lease lifecycle.
//! The Python peer is controlled protocol evidence, never a live AGY/Tier1 claim.
use super::acp_planner_provider::{Root, Stack, boot_as_scenario, requests};
use calm_server::{
    mcp_server::{ToolCallIdentity, build_default_registry},
    model::{CardRole, Task, TaskStatus},
};
use calm_types::{
    acp_worker::{AcpWorkerState, WorkerSelectionSource},
    runtime::AgentProvider,
};
use serde_json::{Value, json};
use std::time::Duration;

async fn boot(root: &Root, scenario: &str) -> Stack {
    boot_as_scenario(root,"antigravity",json!({"ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server","ACP_FIXTURE_VERSION":"1.5.0","ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"}),"1.5.0",scenario).await
}
async fn identity(stack: &Stack, card: &str, role: CardRole) -> ToolCallIdentity {
    let runtime = stack.runtime(card).await;
    let row = stack
        .repo()
        .session_get(&runtime.id.clone().into())
        .await
        .unwrap()
        .unwrap();
    let area = stack
        .repo()
        .track_get(row.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .area_id;
    ToolCallIdentity {
        card_id: card.into(),
        role,
        provider: AgentProvider::Antigravity,
        session_id: runtime.id.clone(),
        track_id: Some(row.track_id.to_string()),
        area_id: area.to_string(),
        thread_id: row.thread_id.unwrap_or(runtime.id),
    }
}
async fn declare(stack: &Stack, payload: Value) -> (String, String) {
    let (track, planner) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    declare_on(stack, &track, &planner, payload).await;
    (track, planner)
}
async fn declare_on(stack: &Stack, track: &str, planner: &str, payload: Value) {
    let id = identity(stack, planner, CardRole::Planner).await;
    crate::support::report_writes::upsert_block(
        &stack.state.mcp_context(),
        &build_default_registry(),
        id,
        json!({"kind":"task","payload":payload}),
    )
    .await
    .unwrap();
    stack.state.scheduler_for_test().poke(track.into());
}
fn payload(key: &str, access: &str) -> Value {
    let mut value = json!({"key":key,"kind":"agent","provider":"antigravity","goal":"Perform the frozen controlled worker goal","context":[],"access":access,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true});
    if access == "read_write" {
        value["no_gate_reason"] =
            json!("Controlled native permission fixture; no repository change is expected");
    }
    value
}
async fn wait_task(stack: &Stack, track: &str, key: &str, status: TaskStatus) -> Task {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if let Some(task) = stack.repo().task_current_get(track, key).await.unwrap() {
                if task.status == status {
                    return task;
                }
                assert!(
                    task.status != TaskStatus::Failed || status == TaskStatus::Failed,
                    "unexpected failure: {:?}",
                    task.status_detail
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("task lifecycle deadline")
}
async fn state(stack: &Stack, task: &Task) -> AcpWorkerState {
    let session: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(task.worker_card_id.as_deref().unwrap())
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
        .await
        .expect("historical Worker session binding");
    let row = stack
        .repo()
        .session_get(&session.into())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_value(row.handle_state_json.unwrap()).unwrap()
}
async fn call(
    stack: &Stack,
    identity: ToolCallIdentity,
    name: &str,
    args: Value,
) -> std::result::Result<Value, String> {
    build_default_registry().lookup(name).unwrap()(stack.state.mcp_context(), identity, args)
        .await
        .map(|result| result.into_structured())
        .map_err(|error| format!("{error:?}"))
}
async fn held(stack: &Stack, task: &Task) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM workspace_leases WHERE card_id=?1 AND state IN ('held','releasing')",
    )
    .bind(task.worker_card_id.as_deref().unwrap())
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}
async fn wait_file(root: &Root, name: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !root.path().join(name).exists() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("fake protocol checkpoint")
}
async fn wait_released(stack: &Stack, task: &Task) {
    tokio::time::timeout(Duration::from_secs(25), async {
        while held(stack, task).await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lease releases only after native cleanup");
}
async fn settle(root: &Root, stack: &Stack, task: &Task) {
    let native = state(stack, task).await.native_session_id.unwrap();
    wait_file(root, &format!("worker-cancel-{native}")).await;
    std::fs::write(
        root.path().join(format!("worker-settle-{native}")),
        "settle",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while held(stack, task).await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lease release after exact native settlement");
}
async fn cancel(root: &Root, stack: &Stack, planner: &str, task: &Task) {
    call(
        stack,
        identity(stack, planner, CardRole::Planner).await,
        "neige_task_cancel",
        json!({"key":task.key,"message":"stop controlled worker"}),
    )
    .await
    .unwrap();
    settle(root, stack, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_model_then_effort_use_declared_option_ids_before_business_prompt() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let mut declaration = payload("explicit", "read_only");
    declaration["model"] = json!("fixture/model-b");
    declaration["reasoning_effort"] = json!("deep");
    let (track, planner) = declare(&stack, declaration).await;
    let task = wait_task(&stack, &track, "explicit", TaskStatus::Running).await;
    let argv = std::fs::read_to_string(root.path().join("argv.jsonl")).unwrap();
    assert!(
        argv.lines()
            .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
            .all(|argv| argv.iter().any(|arg| arg == "--sandbox")),
        "native Worker must keep the AGY sandbox argv"
    );
    let selected = state(&stack, &task).await;
    assert_eq!(selected.effective_model.as_deref(), Some("fixture/model-b"));
    assert_eq!(selected.effective_effort.as_deref(), Some("deep"));
    assert_eq!(
        selected.selection_source.model,
        WorkerSelectionSource::Explicit
    );
    let native = selected.native_session_id.unwrap();
    let setters: Vec<_> = requests(&root, "session/set_config_option")
        .into_iter()
        .filter(|request| request["params"]["sessionId"] == native)
        .collect();
    assert_eq!(setters.len(), 2);
    assert_eq!(setters[0]["params"]["configId"], "declared-model-key");
    assert_eq!(setters[1]["params"]["configId"], "declared-effort-key");
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_defaults_are_recorded_without_planner_defaults() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("defaults", "read_only")).await;
    let task = wait_task(&stack, &track, "defaults", TaskStatus::Running).await;
    let selected = state(&stack, &task).await;
    assert_eq!(selected.effective_model.as_deref(), Some("fixture/model-a"));
    assert_eq!(
        selected.selection_source.model,
        WorkerSelectionSource::ProviderDefaultReported
    );
    assert_eq!(selected.requested_selection.model, None);
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_explicit_model_fails_before_business_prompt() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let mut declaration = payload("bad-model", "read_only");
    declaration["model"] = json!("unavailable/model");
    let (track, _) = declare(&stack, declaration).await;
    let task = wait_task(&stack, &track, "bad-model", TaskStatus::Failed).await;
    assert!(requests(&root, "session/prompt").is_empty());
    wait_released(&stack, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn done_records_once_but_holds_lease_until_native_cancel_settles() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, _) = declare(&stack, payload("done", "read_only")).await;
    let task = wait_task(&stack, &track, "done", TaskStatus::Running).await;
    let worker = identity(
        &stack,
        task.worker_card_id.as_deref().unwrap(),
        CardRole::Worker,
    )
    .await;
    let args = json!({"attempt_id":task.id,"result":{"ok":true}});
    let native = state(&stack, &task).await.native_session_id.unwrap();
    let token = std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap();
    let (mut reader, mut writer) = crate::support::mcp::connect(stack.mcp_socket()).await;
    crate::support::mcp::handshake(&mut reader, &mut writer, &token).await;
    crate::support::mcp::send_frame(&mut writer,json!({"jsonrpc":"2.0","id":80,"method":"tools/call","params":{"name":"neige_task_done","arguments":args}})).await;
    let wire = crate::support::mcp::recv_frame(&mut reader).await;
    assert!(
        wire.get("error").is_none() && wire["result"]["isError"] != true,
        "{wire}"
    );
    assert_eq!(
        wire["result"]["structuredContent"]["status"], "report_received",
        "{wire}"
    );
    call(&stack, worker, "neige_task_done", args).await.unwrap();
    let (mut late_reader, mut late_writer) = crate::support::mcp::connect(stack.mcp_socket()).await;
    crate::support::mcp::send_frame(&mut late_writer, json!({"jsonrpc":"2.0","id":81,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"late-worker","version":"1"},"_meta":{"dev.neige/auth":{"token":token}}}})).await;
    let late = crate::support::mcp::recv_frame(&mut late_reader).await;
    assert!(
        late.get("error").is_some(),
        "revoked token must not establish new authority: {late}"
    );
    let pending = state(&stack, &task).await;
    assert!(pending.release_intent);
    assert_eq!(held(&stack, &task).await, 1);
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='task.completed' AND json_extract(payload,'$.idempotency_key')=?1").bind(&task.id).fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(count, 1);
    settle(&root, &stack, &task).await;
    assert!(!state(&stack, &task).await.release_intent);
    let settled: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='task.report_settled' AND json_extract(payload,'$.idempotency_key')=?1")
        .bind(&task.id).fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='workspace.released' AND json_extract(payload,'$.card_id')=?1")
        .bind(task.worker_card_id.as_deref().unwrap()).fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    let deliveries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM task_git_deliveries WHERE producer_attempt_id=?1")
            .bind(&task.id)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(
        (settled, releases, deliveries),
        (1, 1, 0),
        "one settled report and release; read-only has no delivery"
    );
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_cancel_uses_native_cancel_and_retains_lease_until_settlement() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("cancel", "read_write")).await;
    let task = wait_task(&stack, &track, "cancel", TaskStatus::Running).await;
    let native = state(&stack, &task).await.native_session_id.unwrap();
    std::fs::write(
        root.path().join(format!("worker-late-permission-{native}")),
        "ask after cancel",
    )
    .unwrap();
    call(
        &stack,
        identity(&stack, &planner, CardRole::Planner).await,
        "neige_task_cancel",
        json!({"key":task.key,"message":"stop controlled worker"}),
    )
    .await
    .unwrap();
    let native = state(&stack, &task).await.native_session_id.unwrap();
    wait_file(&root, &format!("worker-cancel-{native}")).await;
    wait_file(&root, &format!("worker-late-answer-{native}")).await;
    let late: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join(format!("worker-late-answer-{native}"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        late["outcome"]["outcome"], "cancelled",
        "a normally allowed read is denied after the cancellation fence"
    );
    assert_eq!(held(&stack, &task).await, 1);
    settle(&root, &stack, &task).await;
    assert_eq!(
        stack
            .repo()
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Canceled
    );
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_dirty_done_refuses_and_retains_report_authority_and_lease() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("dirty", "read_only")).await;
    let task = wait_task(&stack, &track, "dirty", TaskStatus::Running).await;
    let cwd: String =
        sqlx::query_scalar("SELECT path FROM workspace_leases WHERE card_id=?1 AND state='held'")
            .bind(task.worker_card_id.as_deref().unwrap())
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    std::fs::write(
        std::path::Path::new(&cwd).join("worker-untracked.txt"),
        "changed",
    )
    .unwrap();
    let result = call(
        &stack,
        identity(
            &stack,
            task.worker_card_id.as_deref().unwrap(),
            CardRole::Worker,
        )
        .await,
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap_err();
    assert!(result.contains("worker-read-only-changed"), "{result}");
    assert_eq!(held(&stack, &task).await, 1);
    assert_eq!(
        stack
            .repo()
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    std::fs::remove_file(std::path::Path::new(&cwd).join("worker-untracked.txt")).unwrap();
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_turn_without_report_or_notification_fails_without_goal_replay() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-reply").await;
    let (track, _) = declare(&stack, payload("no-report", "read_only")).await;
    let task = wait_task(&stack, &track, "no-report", TaskStatus::Failed).await;
    assert!(
        task.status_detail
            .as_deref()
            .unwrap()
            .starts_with("worker-turn-ended")
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    wait_released(&stack, &task).await;
    stack.shutdown().await;
}
async fn permission_case(access: &str, allowed: &[usize]) {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-permissions").await;
    let (track, _) = declare(&stack, payload("permissions", access)).await;
    let task = wait_task(&stack, &track, "permissions", TaskStatus::Failed).await;
    let answers: Vec<Value> = std::fs::read_to_string(root.path().join("worker-permissions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(answers.len(), 15);
    for (index, answer) in answers.iter().enumerate() {
        assert_eq!(
            answer["answer"]["outcome"]["outcome"],
            if allowed.contains(&index) {
                "selected"
            } else {
                "cancelled"
            },
            "{answer}"
        );
    }
    let asks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='ask.requested'")
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(
        asks, 0,
        "native permission requests never create an interactive ask"
    );
    wait_released(&stack, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_permission_handler_denies_every_native_request() {
    permission_case("read_only", &[]).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_write_allows_only_unique_once_inside_lease_read_and_edit() {
    permission_case("read_write", &[0, 1]).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_messages_are_not_read_on_notification_and_use_the_same_native_session() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("messages", "read_only")).await;
    let task = wait_task(&stack, &track, "messages", TaskStatus::Running).await;
    let old_turn = stack.runtime(task.worker_card_id.as_deref().unwrap()).await;
    let native = state(&stack, &task).await.native_session_id.unwrap();
    let ended = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "acp-worker-round-ended",
        &track,
        ended.clone(),
    );
    std::fs::write(
        root.path().join(format!("worker-release-{native}")),
        "finish first round",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), ended.entered.notified())
        .await
        .unwrap();
    // The actual prompt response has arrived: notification is now queued in the idle gap.
    assert_eq!(state(&stack, &task).await.notified_through_seq, 0);
    let planner_id = identity(&stack, &planner, CardRole::Planner).await;
    let args = json!({"attempt_id":task.id,"idempotency_key":"one-message","action":{"type":"message","text":"Check the additional acceptance condition"}});
    let receipt = call(
        &stack,
        planner_id.clone(),
        "neige_terminal_input",
        args.clone(),
    )
    .await
    .unwrap();
    assert_eq!(receipt["outcome"], "queued");
    assert_eq!(receipt["transport"], "native_session");
    assert!(receipt.get("terminal_id").is_none());
    assert_eq!(
        receipt,
        call(&stack, planner_id, "neige_terminal_input", args)
            .await
            .unwrap()
    );
    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM task_planner_messages WHERE attempt_id=?1 AND read_at_ms IS NULL",
    )
    .bind(&task.id)
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(unread, 1);
    ended.release.notify_one();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let prompts = requests(&root, "session/prompt");
            if prompts.len() >= 2 {
                assert_eq!(
                    prompts[0]["params"]["sessionId"],
                    prompts[1]["params"]["sessionId"]
                );
                let followup = prompts[1]["params"]["prompt"][0]["text"].as_str().unwrap();
                assert!(followup.contains(receipt["message_id"].as_str().unwrap()));
                assert!(!followup.contains("Perform the frozen controlled worker goal"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while state(&stack, &task).await.notified_through_seq != 1 {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();
    let successor = stack.runtime(task.worker_card_id.as_deref().unwrap()).await;
    assert_ne!(old_turn.active_turn_id, successor.active_turn_id);
    let cancels = requests(&root, "session/cancel").len();
    assert!(
        stack
            .state
            .operation_runtime
            .stop_worker_turn_for_test(&old_turn)
            .await
            .is_err()
    );
    assert_eq!(
        requests(&root, "session/cancel").len(),
        cancels,
        "old turn cannot cancel successor"
    );
    let worker = identity(
        &stack,
        task.worker_card_id.as_deref().unwrap(),
        CardRole::Worker,
    )
    .await;
    let refused = call(
        &stack,
        worker.clone(),
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("unread Planner messages"), "{refused}");
    let messages = call(
        &stack,
        worker.clone(),
        "neige_message_read",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap();
    assert_eq!(messages["messages"].as_array().unwrap().len(), 1);
    call(
        &stack,
        worker,
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap();
    settle(&root, &stack, &task).await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_gate_checkpoint_runs_in_active_round_and_finalize_waits_for_report_settlement() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let mut declaration = payload("gate", "read_write");
    declaration["gate"] = json!({"steps":[{"name":"controlled gate","cmd":"true"}]});
    let (track, _) = declare(&stack, declaration).await;
    let task = wait_task(&stack, &track, "gate", TaskStatus::Running).await;
    let worker = identity(
        &stack,
        task.worker_card_id.as_deref().unwrap(),
        CardRole::Worker,
    )
    .await;
    let run = call(
        &stack,
        worker.clone(),
        "neige_task_gate",
        json!({"attempt_id":task.id,"commit_message":"test: controlled ACP Worker checkpoint"}),
    )
    .await
    .unwrap();
    assert_eq!(run["state"], "finished", "{run}");
    assert_eq!(run["passed"], true, "{run}");
    call(
        &stack,
        worker.clone(),
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap();
    assert_eq!(held(&stack, &task).await, 1);
    assert!(
        call(
            &stack,
            worker,
            "neige_task_gate",
            json!({"attempt_id":task.id})
        )
        .await
        .is_err()
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let finalize:i64=sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind='task-verify' AND json_extract(payload_json,'$.task_id')=?1").bind(&task.id).fetch_one(stack.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(finalize, 0);
    settle(&root, &stack, &task).await;
    let _ = wait_task(&stack, &track, "gate", TaskStatus::Done).await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_one_worker_does_not_interrupt_a_sibling_on_the_shared_carrier() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track_a, planner_a) = declare(&stack, payload("first", "read_only")).await;
    let first = wait_task(&stack, &track_a, "first", TaskStatus::Running).await;
    let (track_b, planner_b) = declare(&stack, payload("second", "read_only")).await;
    let second = wait_task(&stack, &track_b, "second", TaskStatus::Running).await;
    cancel(&root, &stack, &planner_a, &first).await;
    assert_eq!(
        stack
            .repo()
            .task_get(&second.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(held(&stack, &second).await, 1);
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0]["_pid"], prompts[1]["_pid"]);
    assert_ne!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    cancel(&root, &stack, &planner_b, &second).await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_changed_head_refuses_done_even_with_a_clean_worktree() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("head", "read_only")).await;
    let task = wait_task(&stack, &track, "head", TaskStatus::Running).await;
    let cwd: String =
        sqlx::query_scalar("SELECT path FROM workspace_leases WHERE card_id=?1 AND state='held'")
            .bind(task.worker_card_id.as_deref().unwrap())
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    let before = state(&stack, &task).await.read_only_target.unwrap().head;
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Controlled Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "controlled changed HEAD",
        ])
        .current_dir(&cwd)
        .status()
        .unwrap();
    assert!(status.success());
    let refusal = call(
        &stack,
        identity(
            &stack,
            task.worker_card_id.as_deref().unwrap(),
            CardRole::Worker,
        )
        .await,
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap_err();
    assert!(refusal.contains("worker-read-only-changed"), "{refusal}");
    assert_eq!(held(&stack, &task).await, 1);
    assert!(
        std::process::Command::new("git")
            .args(["reset", "--soft", &before])
            .current_dir(&cwd)
            .status()
            .unwrap()
            .success()
    );
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}

#[path = "cases/acp_worker_additional.rs"]
mod additional;

#[path = "cases/acp_worker_startup.rs"]
mod startup;

#[path = "cases/acp_worker_timeout_cleanup.rs"]
mod timeout_cleanup;
