use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::event::EventBus;
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::operation::{OperationCompletionBus, OperationKey, OperationRepo, SqlxOperationRepo};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_truth::db::RepoRead;
use calm_truth::session_projection_repo::WorkerSessionProjectionRepo;
use sqlx::Row;
use std::sync::Arc;

struct ClaudeWorkerHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: ClaudeWorkerAdapter,
    wave_id: String,
    events: EventBus,
}

async fn claude_worker_harness() -> ClaudeWorkerHarness {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        repo.as_ref(),
        crate::model::NewCove {
            name: "claude workspace leases".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        repo.as_ref(),
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "claude workspace leases".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    ClaudeWorkerHarness {
        adapter: ClaudeWorkerAdapter::new(
            route_repo,
            Arc::new(CodexClient::new_stub()),
            None,
            CardRoleCache::new(),
            WaveCoveCache::new(),
        ),
        repo,
        wave_id: wave.id.to_string(),
        events: EventBus::new(),
    }
}

fn claude_worker_payload(wave_id: &str, key: &str) -> Value {
    serde_json::to_value(ClaudeWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        wave_id: wave_id.to_string(),
        idempotency_key: format!("{wave_id}:{key}"),
        goal: format!("do {key}"),
        cwd: None,
        context: Value::Null,
        acceptance_criteria: None,
    })
    .unwrap()
}

fn claude_worker_op(id: &str, payload: Value) -> Operation {
    Operation {
        id: id.to_string(),
        operation_key: format!("op-key-{id}"),
        kind: "claude-worker".into(),
        idempotency_key: Some(id.to_string()),
        payload_hash: "hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({ "type": "unknown", "id": null }),
        payload,
        tx_output: None,
        phase: crate::operation::Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

async fn prepare_claude_worker(
    harness: &ClaudeWorkerHarness,
    key: &str,
) -> (TxOutput, Vec<BroadcastEnvelope>, String) {
    let payload = claude_worker_payload(&harness.wave_id, key);
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "claude-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("op-{key}")),
                payload_hash: format!("hash-{key}"),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == op_id)
        .unwrap();
    let claimed_op_id = op.id.clone();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    let events = output.post_commit_events.clone();
    tx.commit().await.unwrap();
    (output, events, claimed_op_id)
}

#[test]
fn claude_worker_command_line_uses_appended_system_prompt_not_mcp_tools() {
    let command = build_claude_worker_command_line(
        "claude",
        Path::new("/tmp/claude-worker/settings.json"),
        "session-1",
        "wave-1",
        "Goal:\ndo the work",
    );

    assert!(command.contains("--append-system-prompt"));
    assert!(
        command.contains("neige task-completed"),
        "worker system prompt must instruct neige CLI completion: {command}"
    );
    assert!(!command.contains("--mcp-config"), "{command}");
    assert!(!command.contains("--allowedTools"), "{command}");
    assert!(!command.contains("mcp__calm__task_complete"), "{command}");
}

#[tokio::test]
async fn claude_worker_prepare_acquires_held_workspace_lease_and_spawn_op() {
    let harness = claude_worker_harness().await;
    let (output, events, op_id) = prepare_claude_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let runtime_id = output.output_string("runtime_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();

    let wave_cwd: String = sqlx::query_scalar("SELECT cwd FROM waves WHERE id = ?1")
        .bind(&harness.wave_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(wave_cwd, "", "regression guard: wave cwd is not a git repo");
    assert_eq!(
        cwd,
        format!(".claude/worktrees/{}/{}", harness.wave_id, card_id)
    );
    assert!(std::path::Path::new(&cwd).is_dir(), "leased cwd exists");
    let lease = sqlx::query(
        "SELECT state, path, card_id, wave_id FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(&lease_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(lease.get::<String, _>("state"), "held");
    assert_eq!(lease.get::<String, _>("path"), cwd);
    assert_eq!(lease.get::<String, _>("card_id"), card_id);
    assert_eq!(lease.get::<String, _>("wave_id"), harness.wave_id);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, Event::WorkspaceLeased { .. }));
    assert!(
        events
            .iter()
            .all(|envelope| envelope.event.kind_tag() != "worktree.provisioned")
    );
    let provisioned_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.provisioned'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(provisioned_events, 0);

    let session = sqlx::query("SELECT provider, spawn_op_id FROM worker_sessions WHERE id = ?1")
        .bind(&runtime_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(session.get::<String, _>("provider"), "claude");
    assert_eq!(
        session
            .get::<Option<String>, _>("spawn_op_id")
            .expect("spawn op id"),
        op_id
    );

    assert!(
        release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
            .await
            .unwrap()
    );
    assert!(
        std::path::Path::new(&cwd).exists(),
        "normal lease release preserves leased cwd"
    );
}

#[tokio::test]
async fn claude_worker_prepare_stores_idempotency_key_in_card_payload() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "payload").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let card = harness
        .repo
        .card_get(&card_id)
        .await
        .unwrap()
        .expect("worker card");

    assert_eq!(
        card.payload.get("idempotency_key").and_then(Value::as_str),
        Some(format!("{}:payload", harness.wave_id).as_str())
    );
    assert_eq!(
        card.payload.get("role_request").and_then(Value::as_str),
        Some("claude")
    );
    assert_eq!(
        card.payload.get("prompt").and_then(Value::as_str),
        output.data.get("prompt").and_then(Value::as_str)
    );

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_worker_spawn_env_carries_raw_card_token_and_socket() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "env").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let runtime_id = output.output_string("runtime_id", "test").unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("kernel.sock");
    let mcp_server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: socket_dir.path().join("neige-mcp-stdio-shim"),
        socket_path: socket_path.clone(),
    });
    let captured_env = Arc::new(tokio::sync::Mutex::new(None::<Value>));
    let captured_env_for_hook = captured_env.clone();
    let hook: SpawnHook = Arc::new(move |_terminal_id, _command_line, _cwd, env| {
        let captured_env = captured_env_for_hook.clone();
        Box::pin(async move {
            *captured_env.lock().await = Some(env);
            Ok(SpawnHandle::NoOp)
        })
    });
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
        route_repo,
        Arc::new(CodexClient::new_stub()),
        Some(mcp_server),
        CardRoleCache::new(),
        WaveCoveCache::new(),
        hook,
    );
    let op_repo: Arc<dyn OperationRepo> =
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let op = claude_worker_op("op-env", Value::Null);

    adapter
        .spawn_side_effect(&output, &op, &ctx)
        .await
        .expect("spawn side effect");

    let env = captured_env
        .lock()
        .await
        .clone()
        .expect("spawn hook captured env");
    assert_eq!(
        env.get("NEIGE_MCP_SOCKET").and_then(Value::as_str),
        Some(socket_path.to_string_lossy().as_ref())
    );
    let raw_token = env
        .get("NEIGE_MCP_TOKEN")
        .and_then(Value::as_str)
        .expect("raw per-card token in spawn env");
    assert!(!raw_token.is_empty());
    let token_hash = crate::mcp_server::auth::hash_token(raw_token);
    let (card_hash, session_hash): (String, Option<String>) = sqlx::query_as(
        r#"SELECT c.hashed_token, ws.mcp_token_hash
                 FROM card_mcp_tokens c
                 JOIN worker_sessions ws ON ws.id = ?2
                WHERE c.card_id = ?1"#,
    )
    .bind(&card_id)
    .bind(&runtime_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(card_hash, token_hash);
    assert_eq!(session_hash.as_deref(), Some(card_hash.as_str()));

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn claude_worker_budget_parallelism_gets_disjoint_lease_paths() {
    let harness = claude_worker_harness().await;
    let (first, _, _) = prepare_claude_worker(&harness, "a").await;
    let (second, _, _) = prepare_claude_worker(&harness, "b").await;
    let first_card = first.output_string("card_id", "test").unwrap();
    let second_card = second.output_string("card_id", "test").unwrap();
    let first_cwd = first.output_string("cwd", "test").unwrap();
    let second_cwd = second.output_string("cwd", "test").unwrap();

    assert_ne!(first_card, second_card);
    assert_ne!(first_cwd, second_cwd);
    assert!(first_cwd.starts_with(".claude/worktrees/"));
    assert!(second_cwd.starts_with(".claude/worktrees/"));

    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE state = 'held'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(held, 2);

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &first_card)
        .await
        .unwrap();
    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &second_card)
        .await
        .unwrap();
}

#[tokio::test]
async fn claude_worker_compensation_cleans_rows_lease_and_settings_dir() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    let runtime_id = output.output_string("runtime_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();
    let settings_path = output.output_string("settings_path", "test").unwrap();
    let settings_dir = settings_path_parent(Path::new(&settings_path)).unwrap();
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(settings_dir.join("settings.json"), "{}").unwrap();

    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let op_repo: Arc<dyn OperationRepo> =
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let ctx = SpawnCtx::new(
        route_repo,
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let raw_token = mint_claude_worker_mcp_token(&ctx, &card_id, &runtime_id)
        .await
        .unwrap();
    assert!(!raw_token.is_empty());

    assert!(harness.repo.card_get(&card_id).await.unwrap().is_some());
    assert!(
        harness
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_some()
    );
    let token_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(token_rows, 1);
    assert!(settings_dir.exists());

    let op = claude_worker_op("op-a", Value::Null);
    let state = harness
        .adapter
        .plan_compensation(PhaseTag::SpawnStarted, "boom", &output, &op)
        .await
        .unwrap();

    assert_eq!(state.steps[0].op, "remove_workspace_artifact");
    assert_eq!(state.steps[1].op, "release_workspace_lease");
    assert_eq!(state.steps[2].op, "cleanup_claude_worker");
    assert_eq!(state.steps[3].op, "delete_claude_settings_dir");
    assert_eq!(
        state.steps[1].arg_string("lease_id", "test").unwrap(),
        lease_id
    );
    assert_eq!(
        state.steps[2].arg_string("card_id", "test").unwrap(),
        card_id
    );
    assert_eq!(
        state.steps[2].arg_string("terminal_id", "test").unwrap(),
        terminal_id
    );

    for step in &state.steps {
        harness
            .adapter
            .compensate_step(step, &output, &op, &ctx)
            .await
            .unwrap();
    }

    assert!(harness.repo.card_get(&card_id).await.unwrap().is_none());
    assert!(
        harness
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_none()
    );
    let session_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(session_rows, 0);
    let token_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(token_rows, 0);
    let lease_state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(lease_state, "released");
    assert!(
        !std::path::Path::new(&cwd).exists(),
        "compensation removes the just-created workspace artifact"
    );
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(removed_events, 1);
    assert!(!settings_dir.exists());
}

#[tokio::test]
async fn claude_worker_recovery_already_exited_returns_noop_without_respawn() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    crate::db::RepoOutOfDomain::terminal_set_exit(
        harness.repo.as_ref(),
        &terminal_id,
        Some(0),
        false,
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let op_repo: Arc<dyn OperationRepo> =
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let ctx = SpawnCtx::new(
        route_repo,
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let op = claude_worker_op("op-a", Value::Null);

    let outcome = harness
        .adapter
        .spawn_side_effect(&output, &op, &ctx)
        .await
        .unwrap();

    assert!(matches!(outcome, SpawnOutcome::Ready(SpawnHandle::NoOp)));
    let token_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens")
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(token_count, 0, "recovery no-op must not mint MCP tokens");
    let runtime = harness
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active claude worker runtime");
    assert_ne!(runtime.status, WorkerSessionState::Starting);
    assert_eq!(runtime.status, WorkerSessionState::Running);
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_worker_recovery_already_live_returns_noop_without_respawn_or_token_rotation() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "already-live").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let runtime_id = output.output_string("runtime_id", "test").unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("kernel.sock");
    let mcp_server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: socket_dir.path().join("neige-mcp-stdio-shim"),
        socket_path,
    });
    let spawn_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let spawn_count_for_hook = spawn_count.clone();
    let hook: SpawnHook = Arc::new(move |_terminal_id, _command_line, _cwd, _env| {
        let spawn_count = spawn_count_for_hook.clone();
        Box::pin(async move {
            spawn_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SpawnHandle::NoOp)
        })
    });
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
        route_repo,
        Arc::new(CodexClient::new_stub()),
        Some(mcp_server),
        CardRoleCache::new(),
        WaveCoveCache::new(),
        hook,
    );
    let op_repo: Arc<dyn OperationRepo> =
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let op = claude_worker_op("op-already-live", Value::Null);

    let first = adapter.spawn_side_effect(&output, &op, &ctx).await.unwrap();

    assert!(matches!(first, SpawnOutcome::Ready(SpawnHandle::NoOp)));
    assert_eq!(spawn_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (initial_card_hash, initial_session_hash): (String, Option<String>) = sqlx::query_as(
        r#"SELECT c.hashed_token, ws.mcp_token_hash
                 FROM card_mcp_tokens c
                 JOIN worker_sessions ws ON ws.id = ?2
                WHERE c.card_id = ?1"#,
    )
    .bind(&card_id)
    .bind(&runtime_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        initial_session_hash.as_deref(),
        Some(initial_card_hash.as_str())
    );
    crate::db::RepoOutOfDomain::terminal_set_pid(harness.repo.as_ref(), &terminal_id, Some(42_424))
        .await
        .unwrap();

    let second = adapter.spawn_side_effect(&output, &op, &ctx).await.unwrap();

    assert!(matches!(second, SpawnOutcome::Ready(SpawnHandle::NoOp)));
    assert_eq!(
        spawn_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "live recovery no-op must not respawn"
    );
    let token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        token_count, 1,
        "live recovery no-op must not mint a new token row"
    );
    let (card_hash, session_hash): (String, Option<String>) = sqlx::query_as(
        r#"SELECT c.hashed_token, ws.mcp_token_hash
                 FROM card_mcp_tokens c
                 JOIN worker_sessions ws ON ws.id = ?2
                WHERE c.card_id = ?1"#,
    )
    .bind(&card_id)
    .bind(&runtime_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(card_hash, initial_card_hash);
    assert_eq!(session_hash, initial_session_hash);
    let runtime = harness
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active claude worker runtime");
    assert_ne!(runtime.status, WorkerSessionState::Starting);
    assert_eq!(runtime.status, WorkerSessionState::Running);
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_worker_fast_exit_preservation_returns_noop_and_marks_runtime_running() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "fast-exit").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("kernel.sock");
    let mcp_server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: socket_dir.path().join("neige-mcp-stdio-shim"),
        socket_path,
    });
    let repo_for_hook = harness.repo.clone();
    let hook: SpawnHook = Arc::new(move |terminal_id, _command_line, _cwd, _env| {
        let repo = repo_for_hook.clone();
        Box::pin(async move {
            crate::db::RepoOutOfDomain::terminal_set_exit(
                repo.as_ref(),
                &terminal_id,
                Some(1),
                false,
            )
            .await
            .unwrap();
            Err(CalmError::Internal("simulated claude fast exit".into()))
        })
    });
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
        route_repo,
        Arc::new(CodexClient::new_stub()),
        Some(mcp_server),
        CardRoleCache::new(),
        WaveCoveCache::new(),
        hook,
    );
    let op_repo: Arc<dyn OperationRepo> =
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let op = claude_worker_op("op-fast-exit", Value::Null);

    let outcome = adapter.spawn_side_effect(&output, &op, &ctx).await.unwrap();

    assert!(matches!(outcome, SpawnOutcome::Ready(SpawnHandle::NoOp)));
    let runtime = harness
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("active claude worker runtime");
    assert_ne!(runtime.status, WorkerSessionState::Starting);
    assert_eq!(runtime.status, WorkerSessionState::Running);
}
