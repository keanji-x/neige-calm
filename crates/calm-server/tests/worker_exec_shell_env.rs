//! Independent acceptance-level regression test for bug #836.
//!
//! A shared-daemon codex **worker** thread must carry
//! `/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET` +
//! `.../NEIGE_MCP_TOKEN`, matching the SPEC path. Without that config, the
//! worker's AI exec-shell never receives the per-card MCP credentials and
//! `neige` reads fail.
//!
//! This test drives the **production WORKER spawn path** end-to-end through
//! the real dispatcher/operation runtime against a live fake codex
//! app-server, captures the inbound `thread/start` request, and asserts the
//! worker `thread/start` carries the same MCP exec-shell env the spec path
//! does. It runs with a LIVE `McpServer` (`mcp_server = Some`) — the
//! production wiring (`state.rs` `new`: `McpServer::spawn` then `Dispatcher`
//! with `Some(mcp_server)`), so the worker spawn hits the
//! config-injecting arm of the #836 fix. On unfixed `main` the worker emits
//! `config: None`, so the captured `thread/start` has no `/params/config` at
//! all and this test is RED. Once the worker path emits the same
//! `shell_environment_policy.set`, it turns GREEN.
//!
//! The harness here mirrors `tests/codex_worker_shared_daemon.rs`
//! (`worker_thread_start_carries_mcp_shell_environment_policy` /
//! `spawn_dispatcher_with_mcp`, which wire a live `McpServer`): same
//! `boot`/`Dispatcher`/`plan_codex_task` wiring, same live shared daemon +
//! `FAKE_CODEX_CAPTURE_REQUESTS` capture file. We must NOT edit that file
//! (owned by the parallel fix agent), and helpers cannot be imported across
//! test binaries, so the shared helpers are replicated here.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::EventBus;
use calm_server::ids::{CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::TOOL_PLAN_UPSERT;
use calm_server::mcp_server::{McpServer, ToolCallIdentity, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("create git repo dir");
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "codex-worker@example.test"]);
    run_git(path, ["config", "user.name", "Codex Worker Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write initial readme");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Boot {
    repo: Arc<dyn Repo>,
    events: EventBus,
    cache: CardRoleCache,
    wcc: calm_server::wave_cove_cache::WaveCoveCache,
    cove_id: CoveId,
    wave_id: WaveId,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    renderer: Arc<TerminalRendererRegistry>,
    shared: Arc<SharedCodexAppServer>,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    spec_card_id: CardId,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo_root = tmp.path().join("wave-repo");
    init_git_repo(&repo_root);
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "worker-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "worker-shared".into(),
            sort: None,
            cwd: repo_root.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "spec".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    seed_spec_session(&sqlx_repo, spec_card.id.as_str()).await;
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();

    let mut codex = CodexClient::new_stub();
    codex.codex_bin = fake_codex_bin();
    let codex = Arc::new(codex);
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let renderer = TerminalRendererRegistry::new_with_repo(route_repo);

    let fake_codex_bin = fake_codex_bin();
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin.as_str(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let shared = SharedCodexAppServer::new_with_pending(&cfg, Arc::new(home), repo.clone(), None);
    shared.start_or_takeover().await.unwrap();

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events: events.clone(),
        write: WriteContext::new(cache.clone(), wcc.clone()),
        daemon_token_hash: None,
        gate_logs_dir: tmp.path().join("gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        repo,
        events,
        cache,
        wcc,
        cove_id: cove.id,
        wave_id: wave.id,
        codex,
        daemon,
        renderer,
        shared,
        ctx,
        registry: Arc::new(registry),
        spec_card_id: spec_card.id,
        _tmp: tmp,
    }
}

async fn seed_spec_session(repo: &SqlxRepo, spec_card_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: "spec-session".to_string(),
            card_id: spec_card_id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some("spec-thread".to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// Spawns a dispatcher whose codex-worker adapter has a real `McpServer`
/// wired in — i.e. `mcp_server = Some`, the PRODUCTION wiring. In production
/// `state.rs` `new` (`McpServer::spawn` at `:867`, then `Dispatcher::spawn_*`
/// with `Some(mcp_server)` at `:978`) ALWAYS hands a live `McpServer` to the
/// dispatcher (boot fails if the spawn fails), so a real worker spawn always
/// hits the `(Some(token), Some(server))` arm of the
/// `spawn_codex_worker_via_shared_daemon` config guard. The `from_parts` test
/// hatch (`state.rs:597/:635/:665`) is the only path that wires `None`; using
/// it here would exercise a `config: None` branch production can never reach,
/// making the #836 assertion a harness-fidelity artifact rather than a real
/// regression check. So mirror the GREEN sibling test
/// (`codex_worker_shared_daemon.rs::spawn_dispatcher_with_mcp`) and wire a live
/// server. The returned `TempDir` owns the bound UDS path and must outlive the
/// dispatcher.
async fn spawn_dispatcher_with_mcp(boot: &Boot) -> (Dispatcher, Arc<McpServer>, TempDir) {
    let tmp = TempDir::new().expect("mcp socket tempdir");
    let socket_path = tmp.path().join("mcp.sock");
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    boot.repo.seed_wave_cove_cache(&wcc).await.unwrap();
    let server = McpServer::spawn(
        boot.repo.clone(),
        boot.events.clone(),
        WriteContext::new(boot.cache.clone(), wcc),
        socket_path,
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        tmp.path().join("gate-logs"),
    )
    .await
    .expect("spawn McpServer");
    let dispatcher = Dispatcher::spawn_with_terminal_renderer(
        boot.repo.clone(),
        boot.events.clone(),
        WriteContext::new(boot.cache.clone(), boot.wcc.clone()),
        boot.codex.clone(),
        boot.daemon.clone(),
        boot.renderer.clone(),
        Some(server.clone()),
        boot.shared.clone(),
        4,
    );
    (dispatcher, server, tmp)
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: "spec-session".to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "spec-thread".into(),
    }
}

/// Drives the SPEC card to plan a `codex` task, which the dispatcher turns
/// into a real `codex-worker` operation → `CodexWorkerAdapter` →
/// `spawn_codex_worker_via_shared_daemon` (the production worker path under
/// test). This is identical to how the real spec agent schedules workers.
async fn plan_codex_task(boot: &Boot, key: &str, goal: &str) {
    let handler = boot
        .registry
        .lookup(TOOL_PLAN_UPSERT)
        .expect("plan upsert registered");
    handler(
        boot.ctx.clone(),
        spec_identity(boot),
        json!({
            "tasks": [{
                "key": key,
                "kind": "codex",
                "goal": goal,
                "context": { "from": "worker-exec-shell-env-test" },
                "acceptance_criteria": "finish",
                "no_gate_reason": "worker exec-shell env regression coverage"
            }],
            "message": "plan worker exec-shell env task"
        }),
    )
    .await
    .expect("plan codex task");
}

/// Polls the fake-codex capture file for the WORKER `thread/start` request.
/// The spec card's own `thread/start` is faked (seeded `spec-session` already
/// has a thread id, so the spec never re-mints), so the only `thread/start`
/// the live daemon actually receives here is the worker's.
async fn wait_for_worker_thread_start(path: &Path) -> Value {
    for _ in 0..250 {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Some(req) = raw
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        {
            return req;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for worker thread/start request in capture file");
}

/// #836: the production shared-daemon worker spawn must carry the MCP
/// exec-shell env (`NEIGE_MCP_SOCKET` + `NEIGE_MCP_TOKEN`) on its
/// `thread/start` request — exactly like the SPEC path does — so the
/// worker's AI exec-shell can run `neige task-completed`.
///
/// RED on unfixed `main`: the worker emits `config: None`, so the captured
/// `thread/start` has no `/params/config` and the pointers resolve to
/// `None`.
#[tokio::test]
async fn worker_thread_start_carries_neige_mcp_exec_shell_env() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }

    let boot = boot().await;
    // Live `McpServer` (mcp_server = Some) — production wiring, so the worker
    // spawn hits the config-injecting arm of the #836 fix. `server`/`_mcp_tmp`
    // own the bound MCP socket + must outlive the worker spawn.
    let (_dispatcher, server, _mcp_tmp) = spawn_dispatcher_with_mcp(&boot).await;
    plan_codex_task(&boot, "worker-mcp-env-1", "prove worker exec-shell env").await;

    let thread_start = wait_for_worker_thread_start(&capture_file).await;

    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    // Sanity: confirm we captured the WORKER thread/start, not a spec one.
    // The worker path renders the Worker-role developer instructions, which
    // include the `neige task-completed` reporting contract. (On main this
    // is already true — the env carrier is the broken part.)
    let developer_instructions = thread_start
        .pointer("/params/developerInstructions")
        .and_then(Value::as_str)
        .expect("worker thread/start must carry developer_instructions");
    assert!(
        developer_instructions.contains("worker agent under spec card"),
        "captured thread/start must be the WORKER spawn (Worker-role prompt): {developer_instructions}"
    );

    // The actual #836 assertions: the worker thread/start must carry the
    // MCP exec-shell env in `shell_environment_policy.set`, mirroring the
    // spec path (`spec_harness_adapters.rs:288/509`).
    let mcp_socket = thread_start
        .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET")
        .and_then(Value::as_str);
    let mcp_token = thread_start
        .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
        .and_then(Value::as_str);

    assert!(
        mcp_socket.is_some_and(|value| !value.is_empty()),
        "#836: worker thread/start must set a non-empty NEIGE_MCP_SOCKET in \
         shell_environment_policy.set — otherwise the worker AI exec-shell \
         cannot reach the MCP socket and `neige task-completed` fails. \
         Captured request: {thread_start}"
    );
    assert_eq!(
        mcp_socket.unwrap(),
        server.shim_config.socket_path.to_string_lossy(),
        "#836: worker thread/start NEIGE_MCP_SOCKET must match the live daemon \
         shim socket. Captured request: {thread_start}"
    );
    assert!(
        mcp_token.is_some_and(|value| !value.is_empty()),
        "#836: worker thread/start must set a non-empty NEIGE_MCP_TOKEN in \
         shell_environment_policy.set — otherwise the worker AI exec-shell \
         cannot authenticate to the MCP socket and `neige task-completed` \
         fails. Captured request: {thread_start}"
    );
}
