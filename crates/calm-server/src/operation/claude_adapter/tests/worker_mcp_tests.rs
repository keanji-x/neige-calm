//! #2470: a Claude task worker's session has the kernel MCP server alone, authenticated as its
//! own Worker card, at its first spawn and at a restart of its card.
use super::*;
use crate::mcp_server::wiring::claude_mcp_config_json;

/// The argv `/bin/sh -c` gives the CLI for `command_line`, as the terminal runs it.
fn shell_argv(command_line: &str) -> Vec<String> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("set -- {command_line}; printf '%s\\0' \"$@\""))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(ToOwned::to_owned)
        .collect()
}

/// The one `--mcp-config` file before `--`, with `--strict-mcp-config` beside it.
fn strict_mcp_config(argv: &[String]) -> PathBuf {
    let options = &argv[..argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len())];
    assert!(
        options.iter().any(|arg| arg == "--strict-mcp-config"),
        "no --strict-mcp-config: {argv:?}"
    );
    let configs: Vec<_> = options
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.starts_with("--mcp-config"))
        .map(|(at, arg)| {
            assert_eq!(arg, "--mcp-config", "{argv:?}");
            PathBuf::from(&options[at + 1])
        })
        .collect();
    assert_eq!(configs.len(), 1, "{argv:?}");
    configs.into_iter().next().unwrap()
}

#[test]
fn claude_worker_command_line_makes_its_card_config_the_only_mcp_config() {
    let command = build_claude_worker_command_line(
        "claude",
        Path::new("/tmp/claude-worker/settings.json"),
        "session-1",
        "track-1",
        "Goal:\ndo the work",
    )
    .unwrap();
    let argv = shell_argv(&command);

    assert_eq!(
        strict_mcp_config(&argv),
        Path::new("/tmp/claude-worker/mcp.json")
    );
    assert_eq!(argv.last().unwrap(), "Goal:\ndo the work", "{argv:?}");
}

#[cfg(feature = "fixtures")]
type Spawned = Arc<tokio::sync::Mutex<Option<(String, Value)>>>;

/// A spawn hook that keeps the command line and env it was handed.
#[cfg(feature = "fixtures")]
fn capturing_hook() -> (Spawned, SpawnHook) {
    let spawned: Spawned = Arc::new(tokio::sync::Mutex::new(None));
    let kept = spawned.clone();
    let hook: SpawnHook = Arc::new(move |_terminal_id, command_line, _cwd, env| {
        let kept = kept.clone();
        Box::pin(async move {
            *kept.lock().await = Some((command_line, env));
            Ok(SpawnHandle::NoOp)
        })
    });
    (spawned, hook)
}

#[cfg(feature = "fixtures")]
fn spawn_ctx(harness: &ClaudeWorkerHarness) -> SpawnCtx {
    SpawnCtx::new(
        harness.repo.clone(),
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone())),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    )
}

/// Asserts the spawn's MCP wiring: the config file holds the kernel shim alone, exactly as the
/// Claude Planner's, and the env's token authenticates, through the kernel's own handshake, as
/// the worker card `card_id` in its Worker role on session `worker_session_id`.
#[cfg(feature = "fixtures")]
async fn assert_kernel_mcp_only(
    harness: &ClaudeWorkerHarness,
    mcp_server: &McpServer,
    (command_line, env): &(String, Value),
    card_id: &str,
    worker_session_id: &str,
) {
    let config = strict_mcp_config(&shell_argv(command_line));
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let planner: Value =
        serde_json::from_str(&claude_mcp_config_json(&mcp_server.shim_config.shim_bin).unwrap())
            .unwrap();
    assert_eq!(written, planner);
    let servers: Vec<_> = written["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(servers, ["neige"]);

    assert_eq!(
        env["NEIGE_MCP_SOCKET"].as_str(),
        mcp_server.shim_config.socket_path.to_str()
    );
    let token = env["NEIGE_MCP_TOKEN"].as_str().expect("card token in env");
    let params = json!({ "_meta": { "dev.neige/auth": { "token": token } } });
    let identity = match crate::mcp_server::handshake::handle_initialize(
        harness.repo.as_ref(),
        None,
        &params,
        "2024-11-05",
    )
    .await
    {
        Ok(ok) => ok.connection_identity,
        Err(error) => panic!("the worker token must authenticate: {error:?}"),
    };
    let crate::mcp_server::registry::ConnectionIdentity::CardBound(card) = identity else {
        panic!("a worker token is card-bound, never daemon trust");
    };
    assert_eq!(card.card_id.as_str(), card_id);
    assert_eq!(card.role, CardRole::Worker);
    assert_eq!(card.provider, AgentProvider::Claude);
    assert_eq!(card.session_id, worker_session_id);
}

#[cfg(feature = "fixtures")]
fn test_mcp_server(dir: &Path) -> Arc<McpServer> {
    McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: dir.join("neige-mcp-stdio-shim"),
        socket_path: dir.join("kernel.sock"),
    })
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_worker_spawn_has_only_the_kernel_mcp_server_as_its_worker_card() {
    let harness = claude_worker_harness().await;
    let (output, _, _) = prepare_claude_worker(&harness, "mcp").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let worker_session_id = output.output_string("runtime_id", "test").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mcp_server = test_mcp_server(dir.path());
    let (spawned, hook) = capturing_hook();
    let adapter = ClaudeWorkerAdapter::new_with_spawn_hook(
        harness.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(mcp_server.clone()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
        hook,
    );
    let op = claude_worker_op("op-mcp", claude_worker_payload(&harness.track_id, "mcp"));

    adapter
        .spawn_side_effect(&output, &op, &spawn_ctx(&harness))
        .await
        .expect("spawn side effect");

    let spawned = spawned.lock().await.clone().expect("spawned");
    assert_kernel_mcp_only(
        &harness,
        &mcp_server,
        &spawned,
        &card_id,
        &worker_session_id,
    )
    .await;
    release_workspace_lease_for_card_repo(
        harness.repo.as_ref(),
        &harness.events,
        &card_id,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
}

/// A task worker's card, its worker exited, is restarted (`--resume`) with the same MCP wiring
/// under the restart's own session, its card token rotated to it.
#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_restart_of_a_worker_card_has_only_the_kernel_mcp_server() {
    use crate::operation::claude_restart_adapter::{
        ClaudeRestartAdapter, ClaudeRestartOperationPayload,
    };
    let harness = claude_worker_harness().await;
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'again', 'claude', 'work', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(format!("{}:again", harness.track_id))
    .bind(&harness.track_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let output = prepare_claude_worker_as_scheduled(&harness, "again").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    let settings_path = output.output_string("settings_path", "test").unwrap();
    crate::db::RepoOutOfDomain::terminal_set_exit(
        harness.repo.as_ref(),
        &terminal_id,
        Some(0),
        false,
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    crate::db::sqlite::session_complete_for_card_tx(&mut tx, &card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mcp_server = test_mcp_server(dir.path());
    let (spawned, hook) = capturing_hook();
    let restart = ClaudeRestartAdapter::new_with_spawn_hook(
        harness.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(mcp_server.clone()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        hook,
    );
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: None,
        card_id: card_id.clone(),
    })
    .unwrap();
    let restart_op = claude_worker_op("op-restart", payload.clone());
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let restart_output = restart
        .prepare_tx(&mut tx, &payload, &restart_op)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let restart_runtime = restart_output.output_string("runtime_id", "test").unwrap();
    restart
        .spawn_side_effect(&restart_output, &restart_op, &spawn_ctx(&harness))
        .await
        .expect("restart spawn side effect");

    let spawned = spawned.lock().await.clone().expect("spawned");
    assert!(
        spawned.0.contains(" --resume "),
        "a restart resumes: {}",
        spawned.0
    );
    assert_eq!(
        strict_mcp_config(&shell_argv(&spawned.0)),
        worker_mcp::mcp_config_path(Path::new(&settings_path)).unwrap(),
        "the restart uses the worker's own config file"
    );
    assert_kernel_mcp_only(&harness, &mcp_server, &spawned, &card_id, &restart_runtime).await;
    release_workspace_lease_for_card_repo(
        harness.repo.as_ref(),
        &harness.events,
        &card_id,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
}
