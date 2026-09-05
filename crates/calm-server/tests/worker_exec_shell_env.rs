//! Independent acceptance-level regression test for bug #836.
//!
//! A shared-daemon codex **worker** thread must carry
//! `/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET` +
//! `.../NEIGE_MCP_TOKEN`, matching the PLANNER path. Without that config, the
//! worker's AI exec-shell never receives the per-card MCP credentials and
//! `neige` reads fail.
//!
//! This test drives the **production WORKER spawn path** end-to-end through
//! the real dispatcher/operation runtime against a live fake codex
//! app-server, captures the inbound `thread/start` request, and asserts the
//! worker `thread/start` carries the same MCP exec-shell env the planner path
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

mod support;

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
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::mcp_server::{McpServer, ToolCallIdentity, ToolRegistry, build_default_registry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_report::TrackReportPayload;
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
    wcc: calm_server::track_area_cache::TrackAreaCache,
    area_id: AreaId,
    track_id: TrackId,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    renderer: Arc<TerminalRendererRegistry>,
    shared: Arc<SharedCodexAppServer>,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    planner_card_id: CardId,
    report_card_id: CardId,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo_root = tmp.path().join("track-repo");
    init_git_repo(&repo_root);
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "worker-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "worker-shared".into(),
            sort: None,
            cwd: repo_root.display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    seed_planner_session(&sqlx_repo, track.id.as_str(), planner_card.id.as_str()).await;
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&wcc).await.unwrap();

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
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events: events.clone(),
        write: WriteContext::new(cache.clone(), wcc.clone()),
        daemon_token_hash: None,
        gate_logs_dir: tmp.path().join("gate-logs"),
        task_budget_default: calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
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
        area_id: area.id,
        track_id: track.id,
        codex,
        daemon,
        renderer,
        shared,
        ctx,
        registry: Arc::new(registry),
        planner_card_id: planner_card.id,
        report_card_id: report_card.id,
        _tmp: tmp,
    }
}

async fn seed_planner_session(repo: &SqlxRepo, track_id: &str, planner_card_id: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: "planner-session".to_string(),
            card_id: planner_card_id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some("planner-thread".to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET root_session_id = 'planner-session' WHERE id = ?1")
        .bind(track_id)
        .execute(&mut *tx)
        .await
        .expect("mark planner session as track root");
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
    let tmp = calm_test_sockets::socket_dir("wes");
    let socket_path = calm_test_sockets::socket_path(tmp.path(), "mcp.sock");
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();
    boot.repo.seed_track_area_cache(&wcc).await.unwrap();
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
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
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
        // #1147 S2 — attached fixtures: materialization on lease is a no-op.
        std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        4,
    );
    (dispatcher, server, tmp)
}

fn planner_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: "planner-session".to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "planner-thread".into(),
    }
}

/// Drives the PLANNER card to plan a `codex` task, which the dispatcher turns
/// into a real `codex-worker` operation → `CodexWorkerAdapter` →
/// `spawn_codex_worker_via_shared_daemon` (the production worker path under
/// test). This is identical to how the real planner agent schedules workers.
async fn write_codex_task_block(boot: &Boot, key: &str, goal: &str) {
    let report = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card");
    let report: TrackReportPayload = serde_json::from_value(report.payload).unwrap();
    let handler = boot
        .registry
        .lookup(TOOL_REPORT_BLOCKS_UPSERT)
        .expect("task block writer registered");
    handler(
        boot.ctx.clone(),
        planner_identity(boot),
        json!({
            "kind": "task",
            "if_doc_rev": report.doc_rev,
            "payload": {
                "key": key,
                "kind": "codex",
                "goal": goal,
                "context": { "from": "worker-exec-shell-env-test" },
                "acceptance": "finish",
                "no_gate_reason": "worker exec-shell env regression coverage",
                "ready": true,
                "declared_by": "spec"
            }
        }),
    )
    .await
    .expect("write codex task block");
}

/// Polls the fake-codex capture file for the WORKER `thread/start` request.
/// The planner card's own `thread/start` is faked (seeded `planner-session` already
/// has a thread id, so the planner never re-mints), so the only `thread/start`
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
/// `thread/start` request — exactly like the PLANNER path does — so the
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
    write_codex_task_block(&boot, "worker-mcp-env-1", "prove worker exec-shell env").await;

    let thread_start = wait_for_worker_thread_start(&capture_file).await;

    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    // Sanity: confirm we captured the WORKER thread/start, not a planner one.
    // The worker path renders the Worker-role developer instructions, which
    // include the `neige task-completed` reporting contract. (On main this
    // is already true — the env carrier is the broken part.)
    let developer_instructions = thread_start
        .pointer("/params/developerInstructions")
        .and_then(Value::as_str)
        .expect("worker thread/start must carry developer_instructions");
    assert!(
        developer_instructions.contains("worker agent under planner card"),
        "captured thread/start must be the WORKER spawn (Worker-role prompt): {developer_instructions}"
    );

    // The actual #836 assertions: the worker thread/start must carry the
    // MCP exec-shell env in `shell_environment_policy.set`, mirroring the
    // planner path (`planner_harness_adapters.rs:288/509`).
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
