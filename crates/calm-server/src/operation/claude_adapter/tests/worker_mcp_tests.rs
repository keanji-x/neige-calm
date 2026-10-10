//! #2470: a Claude task worker's session has the kernel MCP server alone, authenticated as its
//! own Worker card, at its first spawn. #2509: its tools run without a permission prompt, and the
//! task prompt routes the gate through them. (A restart of a task worker's card is refused,
//! #2493: `reader_head_tests.rs` `claude_restart_refuses_a_bound_card`.)
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

/// The one `--mcp-config` file before `--`, with `--strict-mcp-config` beside it and the kernel
/// server's tools allowed without a prompt.
fn strict_mcp_config(argv: &[String]) -> PathBuf {
    let options = &argv[..argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len())];
    assert!(
        options.iter().any(|arg| arg == "--strict-mcp-config"),
        "no --strict-mcp-config: {argv:?}"
    );
    // `--allowedTools` takes a list: an option, not a positional, has to end it.
    let allowed: Vec<_> = options
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.starts_with("--allowedTools") || arg.starts_with("--allowed-tools"))
        .map(|(at, arg)| {
            assert_eq!(arg, "--allowedTools", "{argv:?}");
            assert!(
                options
                    .get(at + 2)
                    .is_some_and(|next| next.starts_with("--")),
                "{argv:?}"
            );
            options[at + 1].as_str()
        })
        .collect();
    assert_eq!(allowed, ["mcp__neige"], "{argv:?}");
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

/// #2509: a gated Claude worker is told to run its gate through the kernel's MCP tool, as the
/// Codex worker is, never through the `neige` CLI.
#[tokio::test]
async fn claude_worker_task_prompt_runs_its_gate_through_the_mcp_tool() {
    let harness = claude_worker_harness().await;
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, gate_json) \
         VALUES (?1, ?2, 'gated', 'claude', 'test', 'null', '[]', 'dispatched', 1, 1, ?3)",
    )
    .bind(format!("{}:gated", harness.track_id))
    .bind(&harness.track_id)
    .bind(json!({ "steps": [{ "name": "check", "cmd": "true" }] }).to_string())
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let (output, _, _) = prepare_claude_worker(&harness, "gated").await;
    let prompt = output.output_string("prompt", "test").unwrap();

    assert!(
        prompt.contains("To run them, call `neige_task_gate` with `commit_message`:"),
        "{prompt}"
    );
    assert!(prompt.contains("\nGate step 1 `check`:"), "{prompt}");
    assert!(!prompt.contains("neige task gate"), "{prompt}");
    assert!(!prompt.contains("--commit-message"), "{prompt}");
    let card_id = output.output_string("card_id", "test").unwrap();
    release_workspace_lease_for_attempt_repo(
        harness.repo.as_ref(),
        &harness.events,
        &super::bound_attempt(&harness.repo, &card_id).await,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
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
    let listed: Vec<_> = crate::mcp_server::build_default_registry()
        .descriptors_listed_for(card.role)
        .into_iter()
        .map(|descriptor| descriptor.name)
        .collect();
    for tool in ["neige_task_done", "neige_task_fail", "neige_task_gate"] {
        assert!(
            listed.iter().any(|name| name == tool),
            "{tool} is not listed for the worker card's role: {listed:?}"
        );
    }
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
    release_workspace_lease_for_attempt_repo(
        harness.repo.as_ref(),
        &harness.events,
        &super::bound_attempt(&harness.repo, &card_id).await,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
}
