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
    // A settings dir the shell would split or unquote unless the path is quoted whole.
    let command = build_claude_worker_command_line(
        "claude",
        Path::new("/tmp/claude worker's dir/settings.json"),
        "session-1",
        "track-1",
        "Goal:\ndo the work",
    )
    .unwrap();
    let argv = shell_argv(&command);

    assert_eq!(
        strict_mcp_config(&argv),
        Path::new("/tmp/claude worker's dir/mcp.json")
    );
    assert_eq!(argv.last().unwrap(), "Goal:\ndo the work", "{argv:?}");
}

#[cfg(feature = "fixtures")]
type Spawned = Arc<tokio::sync::Mutex<Vec<(String, Value)>>>;

/// A spawn hook that keeps the command line and env of every spawn it was handed.
#[cfg(feature = "fixtures")]
fn capturing_hook() -> (Spawned, SpawnHook) {
    let spawned: Spawned = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let kept = spawned.clone();
    let hook: SpawnHook = Arc::new(move |_terminal_id, command_line, _cwd, env| {
        let kept = kept.clone();
        Box::pin(async move {
            kept.lock().await.push((command_line, env));
            Ok(SpawnHandle::NoOp)
        })
    });
    (spawned, hook)
}

/// The card identity `token` authenticates as through the kernel's own `initialize` handshake;
/// `None` when the kernel refuses it.
#[cfg(feature = "fixtures")]
async fn handshake(
    harness: &ClaudeWorkerHarness,
    token: &str,
) -> Option<crate::mcp_server::registry::CardIdentity> {
    let params = json!({ "_meta": { "dev.neige/auth": { "token": token } } });
    let identity = crate::mcp_server::handshake::handle_initialize(
        harness.repo.as_ref(),
        None,
        &params,
        "2024-11-05",
    )
    .await
    .ok()?
    .connection_identity;
    match identity {
        crate::mcp_server::registry::ConnectionIdentity::CardBound(card) => Some(card),
        crate::mcp_server::registry::ConnectionIdentity::DaemonTrust => {
            panic!("a worker token is card-bound, never daemon trust")
        }
    }
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
    let card = handshake(harness, token)
        .await
        .expect("the worker token must authenticate");
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

    let spawned = spawned.lock().await.clone();
    assert_eq!(spawned.len(), 1);
    assert_kernel_mcp_only(
        &harness,
        &mcp_server,
        &spawned[0],
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

#[cfg(feature = "fixtures")]
struct WorkerRestart {
    harness: ClaudeWorkerHarness,
    _mcp_dir: tempfile::TempDir,
    mcp_server: Arc<McpServer>,
    spawned: Spawned,
    restart: crate::operation::claude_restart_adapter::ClaudeRestartAdapter,
    restart_op: Operation,
    restart_output: TxOutput,
    /// The restart's own worker session.
    restart_session: String,
    card_id: String,
    settings_path: String,
    /// The token the exited worker held.
    predecessor_token: String,
}

/// A task worker's card whose worker held a token and exited, with its restart prepared.
#[cfg(feature = "fixtures")]
async fn prepared_worker_restart() -> WorkerRestart {
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
    let first_session = harness
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("the worker's session")
        .id;
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    let settings_path = output.output_string("settings_path", "test").unwrap();
    let predecessor_token =
        mint_claude_worker_mcp_token(&spawn_ctx(&harness), &card_id, &first_session)
            .await
            .unwrap();
    assert!(handshake(&harness, &predecessor_token).await.is_some());
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

    let mcp_dir = tempfile::tempdir().unwrap();
    let mcp_server = test_mcp_server(mcp_dir.path());
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
    let restart_session = restart_output.output_string("runtime_id", "test").unwrap();
    WorkerRestart {
        harness,
        _mcp_dir: mcp_dir,
        mcp_server,
        spawned,
        restart,
        restart_op,
        restart_output,
        restart_session,
        card_id,
        settings_path,
        predecessor_token,
    }
}

#[cfg(feature = "fixtures")]
async fn release(harness: &ClaudeWorkerHarness, card_id: &str) {
    release_workspace_lease_for_card_repo(
        harness.repo.as_ref(),
        &harness.events,
        card_id,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
}

/// A task worker's card, its worker exited, is restarted (`--resume`) with the same MCP wiring
/// under the restart's own session: the card token rotates to it, so the exited worker's no
/// longer authenticates.
#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_restart_of_a_worker_card_has_only_the_kernel_mcp_server() {
    let r = prepared_worker_restart().await;
    r.restart
        .spawn_side_effect(&r.restart_output, &r.restart_op, &spawn_ctx(&r.harness))
        .await
        .expect("restart spawn side effect");

    let spawned = r.spawned.lock().await.clone();
    assert_eq!(spawned.len(), 1);
    assert!(
        spawned[0].0.contains(" --resume "),
        "a restart resumes: {}",
        spawned[0].0
    );
    assert_eq!(
        strict_mcp_config(&shell_argv(&spawned[0].0)),
        worker_mcp::mcp_config_path(Path::new(&r.settings_path)).unwrap(),
        "the restart uses the worker's own config file"
    );
    assert_kernel_mcp_only(
        &r.harness,
        &r.mcp_server,
        &spawned[0],
        &r.card_id,
        &r.restart_session,
    )
    .await;
    assert!(
        handshake(&r.harness, &r.predecessor_token).await.is_none(),
        "the exited worker's token is refused after the restart"
    );
    release(&r.harness, &r.card_id).await;
}

/// The restart's spawn is driven again after its child started (a crash or lost lease before the
/// spawn was recorded): the live child keeps its token, and no second child is spawned.
#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_restart_redrive_keeps_the_live_childs_token() {
    let r = prepared_worker_restart().await;
    let terminal_id = r
        .restart_output
        .output_string("terminal_id", "test")
        .unwrap();
    r.restart
        .spawn_side_effect(&r.restart_output, &r.restart_op, &spawn_ctx(&r.harness))
        .await
        .expect("restart spawn side effect");
    // What the real spawn records of the child it started.
    crate::db::RepoOutOfDomain::terminal_set_pid(r.harness.repo.as_ref(), &terminal_id, Some(4242))
        .await
        .unwrap();
    let live_token = r.spawned.lock().await[0].1["NEIGE_MCP_TOKEN"]
        .as_str()
        .unwrap()
        .to_string();

    r.restart
        .spawn_side_effect(&r.restart_output, &r.restart_op, &spawn_ctx(&r.harness))
        .await
        .expect("re-driven restart spawn side effect");

    let card = handshake(&r.harness, &live_token)
        .await
        .expect("the live child's token still authenticates");
    assert_eq!(card.card_id.as_str(), r.card_id);
    assert_eq!(card.session_id, r.restart_session);
    assert_eq!(
        r.spawned.lock().await.len(),
        1,
        "no second child is spawned"
    );
    release(&r.harness, &r.card_id).await;
}
