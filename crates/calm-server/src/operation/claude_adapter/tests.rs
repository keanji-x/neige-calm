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

#[path = "test_support.rs"]
mod support;
use support::*;

#[test]
fn claude_worker_command_line_uses_appended_system_prompt_not_mcp_tools() {
    let command = build_claude_worker_command_line(
        "claude",
        Path::new("/tmp/claude-worker/settings.json"),
        "session-1",
        "track-1",
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

    let track_cwd: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id = ?1")
        .bind(&harness.track_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(track_cwd, harness.workspace.path().to_str().unwrap());
    assert_eq!(
        Path::new(&cwd),
        harness
            .workspace
            .path()
            .join(".claude/worktrees")
            .join(&harness.track_id)
            .join(&card_id)
    );
    assert!(
        !Path::new(&cwd).exists(),
        "prepare must not present an empty directory as a checkout"
    );
    let lease = sqlx::query(
        "SELECT state, path, card_id, track_id FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(&lease_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(lease.get::<String, _>("state"), "held");
    assert_eq!(lease.get::<String, _>("path"), cwd);
    assert_eq!(lease.get::<String, _>("card_id"), card_id);
    assert_eq!(lease.get::<String, _>("track_id"), harness.track_id);
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
        !Path::new(&cwd).exists(),
        "releasing a prepared lease does not create a checkout"
    );
}

/// #1814: a task-dispatched Claude worker runs with the owner's config dir, so its auto-memory is
/// off; the env the spawn side effect hands the PTY is the prepared one.
#[tokio::test]
async fn claude_worker_env_disables_claude_auto_memory() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "memory").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    assert_eq!(
        output.data["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1",
        "{}",
        output.data["env"]
    );
    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
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
        Some(format!("{}:payload", harness.track_id).as_str())
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
    let hook: SpawnHook = Arc::new(move |_terminal_id, _command_line, cwd, env| {
        let captured_env = captured_env_for_hook.clone();
        Box::pin(async move {
            assert_eq!(
                std::fs::read_to_string(Path::new(&cwd).join("worker-source")).unwrap(),
                "tracked source",
                "Claude must start in a provisioned checkout containing Track source"
            );
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
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
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
    let op = claude_worker_op("op-env", claude_worker_payload(&harness.track_id, "env"));

    adapter
        .spawn_side_effect(&output, &op, &ctx)
        .await
        .expect("spawn side effect");

    workspace::provision(&adapter, &ctx, &output).await.unwrap();
    let ready_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.provisioned'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(ready_events, 1, "provisioning recovery is idempotent");
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
    assert!(Path::new(&first_cwd).starts_with(harness.workspace.path().join(".claude/worktrees")));
    assert!(Path::new(&second_cwd).starts_with(harness.workspace.path().join(".claude/worktrees")));

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
    workspace::provision(&harness.adapter, &ctx, &output)
        .await
        .unwrap();
    assert!(Path::new(&cwd).join("worker-source").is_file());
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

/// A worker op frozen before the base was recorded recovers unpinned.
/// Its `tx_output` has `repo_root` / `slice_branch` / `cwd` but no `base_sha`
/// or `canonical_path` (design D12 (d)): provisioning must succeed and take
/// today's shape — the repository's HEAD at spawn time, no check against a
/// base it never recorded — rather than refuse the recovery.
#[tokio::test]
async fn pre_slice1_frozen_worker_op_provisions_unpinned() {
    let harness = claude_worker_harness().await;
    let (prepared, _, _) = prepare_claude_worker(&harness, "frozen").await;
    let card_id = prepared.output_string("card_id", "test").unwrap();
    let cwd = prepared.output_string("cwd", "test").unwrap();
    let recorded_base = prepared.output_string("base_sha", "test").unwrap();
    assert!(
        prepared.output_string("canonical_path", "test").is_ok(),
        "a lease prepared by this slice freezes canonical_path"
    );

    // The frozen shape from before slice 1: strip what the slice added.
    let mut frozen = prepared.clone();
    let data = frozen.data.as_object_mut().unwrap();
    data.remove("base_sha");
    data.remove("canonical_path");
    assert!(
        frozen
            .output_optional_string("base_sha", "test")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        frozen.output_string("repo_root", "test").unwrap(),
        prepared.output_string("repo_root", "test").unwrap()
    );

    // The repository moves on between the (old) prepare and this recovery.
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "moved after the frozen prepare",
            ])
            .current_dir(harness.workspace.path())
            .status()
            .unwrap()
            .success()
    );
    let moved_head = git_head(harness.workspace.path());
    assert_ne!(moved_head, recorded_base, "test setup moved HEAD");

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
    workspace::provision(&harness.adapter, &ctx, &frozen)
        .await
        .expect("a pre-slice-1 frozen op provisions without a base");

    assert!(Path::new(&cwd).join("worker-source").is_file());
    assert_eq!(
        git_head(Path::new(&cwd)),
        moved_head,
        "unpinned: the worktree follows the HEAD at spawn time, as before slice 1"
    );
    let provisioned_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.provisioned'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(provisioned_events, 1);

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
}

/// The production spawn path (`workspace::provision`, which reads the frozen
/// `tx_output`) provisions at the base the prepare tx
/// recorded, not at the HEAD the attached repository has moved on to.
#[tokio::test]
async fn claude_spawn_provisions_at_frozen_base_not_moving_head() {
    let harness = claude_worker_harness().await;
    let (prepared, _, _) = prepare_claude_worker(&harness, "pinned").await;
    let card_id = prepared.output_string("card_id", "test").unwrap();
    let cwd = prepared.output_string("cwd", "test").unwrap();
    let recorded_base = prepared.output_string("base_sha", "test").unwrap();

    // The repository moves on between prepare and spawn.
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "moved after prepare",
            ])
            .current_dir(harness.workspace.path())
            .status()
            .unwrap()
            .success()
    );
    let moved_head = git_head(harness.workspace.path());
    assert_ne!(moved_head, recorded_base, "test setup moved HEAD");

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
    workspace::provision(&harness.adapter, &ctx, &prepared)
        .await
        .expect("spawn provisions the prepared lease");

    assert_eq!(
        git_head(Path::new(&cwd)),
        recorded_base,
        "the worktree starts at the frozen base"
    );
    assert_ne!(
        git_head(Path::new(&cwd)),
        moved_head,
        "not at the HEAD that moved after prepare"
    );

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
}

fn git_head(dir: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
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
    let op = claude_worker_op(
        "op-already-live",
        claude_worker_payload(&harness.track_id, "already-live"),
    );

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
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
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
    let op = claude_worker_op(
        "op-fast-exit",
        claude_worker_payload(&harness.track_id, "fast-exit"),
    );

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

#[tokio::test]
async fn claude_worker_prepare_titles_card_with_task_key() {
    let harness = claude_worker_harness().await;
    let (output, _events, _op_id) = prepare_claude_worker(&harness, "slice-c").await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, Some("slice-c".to_string()));

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(wire.title, Some("slice-c".to_string()));
}

#[tokio::test]
async fn claude_worker_prompt_includes_completion_task_id() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "identity").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
        .await
        .unwrap();
    assert!(
        output
            .output_string("prompt", "test")
            .unwrap()
            .contains(&format!("{}:identity", harness.track_id)),
        "the worker must receive the task id it is required to echo"
    );
}

#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod launch_cleanup_tests;

#[cfg(test)]
mod upstream_tests;

/// #1727 S4 slice 2 — the lease the claude worker's `prepare_tx` takes is a kernel-delivery
/// lease (`delivery_policy = 'kernel'`, written in the same INSERT as its base); the
/// fixtures-only plain lease stays NULL (legacy).
#[tokio::test]
async fn worker_lease_is_kernel_policy() {
    let harness = claude_worker_harness().await;
    let (prepared, _, _) = prepare_claude_worker(&harness, "kernel").await;
    let card_id = prepared.output_string("card_id", "test").unwrap();
    let lease_id: String =
        sqlx::query_scalar("SELECT lease_id FROM workspace_leases WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    let policy: Option<String> =
        sqlx::query_scalar("SELECT delivery_policy FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(policy.as_deref(), Some("kernel"));
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let lease =
        crate::operation::workspace_lease::facts::workspace_lease_by_id_tx(&mut tx, &lease_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        lease.delivery_policy,
        Some(crate::operation::workspace_lease::DeliveryPolicy::Kernel)
    );
    assert!(lease.base.is_some());

    let plain_card = format!("{card_id}-plain");
    let (plain, _event) = crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        &plain_card,
        &harness.track_id,
        "op-plain",
        &harness.workspace.path().join("plain-lease"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(plain.delivery_policy, None);
    let policy: Option<String> =
        sqlx::query_scalar("SELECT delivery_policy FROM workspace_leases WHERE card_id = ?1")
            .bind(&plain_card)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(policy, None, "the plain fixture lease is legacy");
}
