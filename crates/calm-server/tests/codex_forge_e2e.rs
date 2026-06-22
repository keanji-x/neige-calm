//! Real Codex forge E2E for issue #760.
//!
//! Feature-gated behind `codex-e2e` and self-skipping when no real Codex
//! binary is available. The test keeps GitHub fake via a local `gh` shim, but
//! runs a real local Codex app-server and a real Codex worker against a local
//! bare git origin. The worker must write a small file and call the
//! `git.commit` MCP forge tool.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::TOOL_PLAN_UPSERT;
use calm_server::mcp_server::{
    McpServer, ToolCallIdentity, ToolRegistry, auth, build_default_registry,
};
use calm_server::model::{CardRole, NewCard, NewCove, NewPlugin, NewWave, now_ms};
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
    TxOutput,
};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::{SharedCodexAppServer, SharedDaemonState};
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";
const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
const PLUGIN_ID: &str = "dev.neige.git-forge";
const COMMIT_PLUGIN_IDEM: &str = "git.commit:forge-e2e-commit";
const TASK_KEY: &str = "forge-e2e";
const SPEC_SESSION_ID: &str = "codex-forge-e2e-spec-session";

static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

const GOAL: &str = r#"Goal: Create a single new file `FORGE_E2E.md` at the repository root containing exactly the line `forge-e2e-ok`. Then commit it using the `git.commit` MCP tool with message `forge-e2e: add marker` and idem `forge-e2e-commit`. Do not modify any other file. Do not run `git push`. Do not open a PR.
Acceptance: `FORGE_E2E.md` exists with that content and exactly one commit was made via `git.commit`."#;

#[allow(dead_code)]
struct Fixture {
    server: Arc<McpServer>,
    plugin_host: Arc<PluginHost>,
    repo: Arc<SqlxRepo>,
    repo_dyn: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    shared: Arc<SharedCodexAppServer>,
    runtime: Arc<OperationRuntime>,
    renderer: Arc<TerminalRendererRegistry>,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    wave_cwd: PathBuf,
    origin_repo: PathBuf,
    origin_main_initial: String,
    codex_stderr_log: PathBuf,
    _forge_env: ForgeTestEnv,
    _codex_path: EnvGuard,
    _proxy_env: ProxyEnv,
    _tmp: TempDir,
    _socket_tmp: TempDir,
}

#[tokio::test]
async fn real_codex_worker_writes_and_commits_via_git_commit_tool() {
    let Some(codex_bin) = resolve_codex_bin() else {
        eprintln!("[codex-forge-e2e] SKIP: no codex bin");
        return;
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let fx = match boot_real_codex_worker_fixture(codex_bin).await {
        Ok(fx) => fx,
        Err(reason) => {
            eprintln!("[codex-forge-e2e] SKIP: {reason}");
            return;
        }
    };

    let _dispatcher = spawn_dispatcher(&fx);
    plan_codex_task(&fx, TASK_KEY, GOAL).await;

    let budget = e2e_budget();
    let task_id = task_id(&fx, TASK_KEY);
    wait_for_worker_commit_side_effect(&fx, &task_id, budget).await;

    let worker = worker_operation_for_task(&fx.repo, &task_id)
        .await
        .unwrap_or_else(|| panic!("codex-worker operation for task {task_id} was not persisted"));
    assert_eq!(
        worker.phase, "succeeded",
        "codex-worker operation {} did not succeed; last_error={:?}",
        worker.id, worker.last_error,
    );
    let output = worker
        .tx_output
        .as_ref()
        .expect("codex-worker tx_output persisted");
    let worker_card_id = output_string(output, "card_id");
    let worker_cwd = PathBuf::from(output_string(output, "cwd"));

    let exact_commit_idem = format!(
        "{PLUGIN_ID}:{}:{worker_card_id}:{COMMIT_PLUGIN_IDEM}",
        fx.wave_id.as_str()
    );
    let commit_op = operation_for_idem(&fx.repo, FORGE_ACTION_KIND, &exact_commit_idem)
        .await
        .unwrap_or_else(|| panic!("missing forge-action operation {exact_commit_idem}"));
    assert_eq!(
        commit_op.phase, "succeeded",
        "git.commit forge-action operation {} did not succeed; last_error={:?}",
        commit_op.id, commit_op.last_error,
    );

    assert_worker_git_state(&fx, &worker_cwd).await;

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

async fn boot_real_codex_worker_fixture(codex_bin: PathBuf) -> Result<Fixture, String> {
    let forge_env = setup_forge_env();
    let codex_path = codex_bin
        .parent()
        .map(prepend_to_path)
        .map(|path| EnvGuard::set("PATH", path))
        .ok_or_else(|| format!("codex binary has no parent: {}", codex_bin.display()))?;
    let proxy_env = apply_proxy_env();

    let tmp = short_tempdir("cf").expect("tempdir");
    let socket_tmp = socket_tempdir().expect("MCP socket tempdir");
    let socket_path = socket_tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let wave_cwd = tmp.path().join("wave-cwd");
    let origin_repo = tmp.path().join("origin.git");

    init_bare_origin(&origin_repo, &tmp.path().join("seed"));
    clone_for_wave(&origin_repo, &wave_cwd);
    let origin_main_initial =
        git_stdout_no_cwd(["--git-dir", path_str(&origin_repo), "rev-parse", "main"]);

    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo_dyn: Arc<dyn Repo> = sqlx_repo.clone();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let write = WriteContext::new(cache.clone(), wave_cove_cache.clone());
    let proxy = active_proxy_value();
    if let Some(proxy) = proxy.as_deref() {
        repo_dyn
            .settings_upsert("http_proxy", proxy)
            .await
            .expect("seed http proxy setting");
        repo_dyn
            .settings_upsert("https_proxy", proxy)
            .await
            .expect("seed https proxy setting");
    }

    let cove = repo_dyn
        .cove_create(NewCove {
            name: "codex-forge-e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo_dyn
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "codex-forge-e2e".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    repo_dyn
        .seed_wave_cove_cache(&wave_cove_cache)
        .await
        .expect("seed wave/cove cache");
    repo_dyn
        .seed_card_role_cache(&cache)
        .await
        .expect("seed card-role cache");
    let spec_card = repo_dyn
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("create spec card");
    cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    seed_spec_session(&sqlx_repo, spec_card.id.as_str()).await;

    let plugin_host = boot_plugin_host(
        repo_dyn.clone(),
        plugins_dir,
        plugins_data_dir,
        events.clone(),
        write.clone(),
    )
    .await;
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host).await;
    emit_workflow_registered_events_for_fixture(
        &repo_dyn,
        &events,
        &cache,
        &wave_cove_cache,
        &plugin_host,
    )
    .await;

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    let operation_runtime_cell = Arc::new(OnceCell::new());
    let daemon_token = auth::CardMcpToken::generate().into_inner();
    let daemon_token_hash = auth::hash_token(&daemon_token);
    let server = McpServer::spawn(
        repo_dyn.clone(),
        events.clone(),
        write.clone(),
        socket_path,
        locate_shim_bin(),
        build_default_registry(),
        Some(daemon_token_hash.clone()),
        plugin_host_cell.clone(),
        operation_runtime_cell.clone(),
        tmp.path().join("gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().expect("tempdir utf8"),
        "--codex-bin",
        codex_bin.to_str().expect("codex path utf8"),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = Arc::new(SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    ));
    seed_auth_only(home.as_ref());
    home.ensure_daemon_mcp_config(&server.shim_config, &daemon_token)
        .expect("write shared daemon MCP config");
    assert_daemon_mcp_config(home.path(), &server.shim_config.socket_path);
    preflight_initialize_through_shim(&server.shim_config.socket_path, &daemon_token).await;

    let shared = SharedCodexAppServer::new_with_pending(&cfg, home.clone(), repo_dyn.clone(), None);
    let codex_stderr_log = cfg
        .shared_codex_appserver_log_dir_resolved()
        .join("stderr.log");
    if let Err(e) = shared.start_or_takeover().await {
        return Err(format!(
            "shared codex app-server did not boot; likely no codex auth in this env: {e}; stderr:\n{}",
            read_lossy(&codex_stderr_log)
        ));
    }
    if !matches!(shared.status_snapshot().state, SharedDaemonState::Running) {
        return Err(format!(
            "shared codex app-server exited during boot; stderr:\n{}",
            read_lossy(&codex_stderr_log)
        ));
    }

    let mut codex = CodexClient::new(&cfg);
    codex.codex_bin = codex_bin.display().to_string();
    let codex = Arc::new(codex);
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let route_repo: Arc<dyn RouteRepo> = repo_dyn.clone();
    let renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let operation_repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![
                Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>,
                Arc::new(CodexWorkerAdapter::new(
                    route_repo.clone(),
                    codex.clone(),
                    shared.clone(),
                    Some(server.clone()),
                    cache.clone(),
                    wave_cove_cache.clone(),
                )) as Arc<dyn ProviderAdapter>,
            ],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo.clone(),
                operation_repo,
                daemon.clone(),
                renderer.clone(),
                events.clone(),
                completion,
            )
            .with_shared_codex_appserver(shared.clone()),
        )
        .await
        .expect("operation runtime"),
    );
    assert!(operation_runtime_cell.set(runtime.clone()).is_ok());

    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: sqlx_repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events: events.clone(),
        write: write.clone(),
        daemon_token_hash: Some(daemon_token_hash),
        gate_logs_dir: tmp.path().join("gate-logs"),
        plugin_host: plugin_host_cell,
        operation_runtime: operation_runtime_cell,
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Ok(Fixture {
        server,
        plugin_host,
        repo: sqlx_repo,
        repo_dyn,
        events,
        write,
        cache,
        wave_cove_cache,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        codex,
        daemon,
        shared,
        runtime,
        renderer,
        ctx,
        registry: Arc::new(registry),
        wave_cwd,
        origin_repo,
        origin_main_initial,
        codex_stderr_log,
        _forge_env: forge_env,
        _codex_path: codex_path,
        _proxy_env: proxy_env,
        _tmp: tmp,
        _socket_tmp: socket_tmp,
    })
}

fn spawn_dispatcher(fx: &Fixture) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_operation_runtime(
        fx.repo_dyn.clone(),
        fx.events.clone(),
        fx.write.clone(),
        fx.codex.clone(),
        fx.daemon.clone(),
        fx.renderer.clone(),
        Some(fx.server.clone()),
        fx.shared.clone(),
        fx.runtime.clone(),
        4,
    )
}

async fn plan_codex_task(fx: &Fixture, key: &str, goal: &str) {
    let handler = fx
        .registry
        .lookup(TOOL_PLAN_UPSERT)
        .expect("plan upsert registered");
    handler(
        fx.ctx.clone(),
        spec_identity(fx),
        json!({
            "tasks": [{
                "key": key,
                "kind": "codex",
                "goal": goal,
                "context": { "from": "codex-forge-e2e" },
                "acceptance_criteria": "FORGE_E2E.md exists and git.commit created exactly one commit",
                "no_gate_reason": "real-codex forge E2E"
            }],
            "message": "plan real codex forge worker"
        }),
    )
    .await
    .expect("plan codex task");
}

fn spec_identity(fx: &Fixture) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: fx.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: SPEC_SESSION_ID.to_string(),
        wave_id: Some(fx.wave_id.as_str().to_string()),
        cove_id: fx.cove_id.as_str().to_string(),
        thread_id: "spec-thread".into(),
    }
}

fn task_id(fx: &Fixture, key: &str) -> String {
    format!("{}:{key}", fx.wave_id.as_str())
}

async fn wait_for_worker_commit_side_effect(fx: &Fixture, task_id: &str, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        if task_completed_exists(&fx.repo, task_id).await {
            return;
        }
        if let Some(reason) = task_failed_reason(&fx.repo, task_id).await {
            panic_with_debug(
                fx,
                task_id,
                format!("task failed before commit assertion: {reason}"),
            )
            .await;
        }
        let worker = worker_operation_for_task(&fx.repo, task_id).await;
        if let Some(worker) = worker.as_ref() {
            if worker.phase == "failed" || worker.phase == "stuck" {
                panic_with_debug(
                    fx,
                    task_id,
                    format!("codex-worker operation ended in {}", worker.phase),
                )
                .await;
            }
            if worker.phase == "succeeded"
                && operation_for_idem_suffix(&fx.repo, FORGE_ACTION_KIND, COMMIT_PLUGIN_IDEM)
                    .await
                    .is_some_and(|row| row.phase == "succeeded")
            {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic_with_debug(
                fx,
                task_id,
                format!("timed out after {budget:?} waiting for worker commit side effect"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn assert_worker_git_state(fx: &Fixture, worker_cwd: &Path) {
    let marker = worker_cwd.join("FORGE_E2E.md");
    let contents = std::fs::read_to_string(&marker)
        .unwrap_or_else(|e| panic!("read marker file {}: {e}", marker.display()));
    assert!(
        contents == "forge-e2e-ok\n" || contents == "forge-e2e-ok",
        "FORGE_E2E.md content mismatch: {contents:?}",
    );

    let head = run_git_capture(worker_cwd, ["rev-parse", "HEAD"]);
    assert_eq!(head.len(), 40, "HEAD must be a full commit hash: {head}");
    let origin_main = run_git_capture(worker_cwd, ["rev-parse", "origin/main"]);
    assert_ne!(head, origin_main, "HEAD must differ from origin/main");
    let rev_count = run_git_capture(worker_cwd, ["rev-list", "--count", "origin/main..HEAD"]);
    assert_eq!(
        rev_count, "1",
        "worker branch must contain exactly one new commit"
    );
    let changed = run_git_capture(worker_cwd, ["diff", "--name-only", "origin/main..HEAD"]);
    assert_eq!(changed, "FORGE_E2E.md", "only the marker file may change");
    let status = run_git_capture(worker_cwd, ["status", "--short", "--untracked-files=all"]);
    assert_eq!(status, "", "worker worktree must be clean after git.commit");
    let bare_main =
        git_stdout_no_cwd(["--git-dir", path_str(&fx.origin_repo), "rev-parse", "main"]);
    assert_eq!(
        bare_main, fx.origin_main_initial,
        "local bare origin main changed; worker must not push"
    );
}

async fn panic_with_debug(fx: &Fixture, task_id: &str, reason: String) -> ! {
    let worker_cwd = worker_operation_for_task(&fx.repo, task_id)
        .await
        .and_then(|row| row.tx_output)
        .map(|output| PathBuf::from(output_string(&output, "cwd")));
    let git_status = worker_cwd
        .as_deref()
        .map(git_status_for_debug)
        .unwrap_or_else(|| "<worker cwd not yet known>".to_string());
    panic!(
        "{reason}\n\ncodex stderr:\n{}\n\nworker git status:\n{}",
        read_lossy(&fx.codex_stderr_log),
        git_status
    );
}

fn git_status_for_debug(cwd: &Path) -> String {
    if !cwd.exists() {
        return format!("{} does not exist", cwd.display());
    }
    let status = run_git_output(
        Some(cwd),
        ["status", "--short", "--branch", "--untracked-files=all"],
    );
    format!(
        "cwd: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    )
}

async fn worker_operation_for_task(repo: &SqlxRepo, task_id: &str) -> Option<OperationRow> {
    operation_for_idem(repo, "codex-worker", task_id).await
}

async fn operation_for_idem(
    repo: &SqlxRepo,
    kind: &str,
    idempotency_key: &str,
) -> Option<OperationRow> {
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, phase, tx_output_json, last_error \
           FROM operations \
          WHERE kind = ?1 AND idempotency_key = ?2 \
          ORDER BY created_at_ms DESC \
          LIMIT 1",
    )
    .bind(kind)
    .bind(idempotency_key)
    .fetch_optional(repo.pool())
    .await
    .expect("operation row query");
    row.map(operation_row_from_tuple)
}

async fn operation_for_idem_suffix(
    repo: &SqlxRepo,
    kind: &str,
    idempotency_suffix: &str,
) -> Option<OperationRow> {
    let like = format!("%:{idempotency_suffix}");
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, phase, tx_output_json, last_error \
           FROM operations \
          WHERE kind = ?1 AND (idempotency_key = ?2 OR idempotency_key LIKE ?3) \
          ORDER BY created_at_ms DESC \
          LIMIT 1",
    )
    .bind(kind)
    .bind(idempotency_suffix)
    .bind(like)
    .fetch_optional(repo.pool())
    .await
    .expect("operation suffix row query");
    row.map(operation_row_from_tuple)
}

fn operation_row_from_tuple(
    (id, phase, tx_output_json, last_error): (String, String, Option<String>, Option<String>),
) -> OperationRow {
    OperationRow {
        id,
        phase,
        tx_output: tx_output_json
            .as_deref()
            .map(|raw| serde_json::from_str(raw).expect("tx_output json")),
        last_error,
    }
}

#[derive(Debug)]
struct OperationRow {
    id: String,
    phase: String,
    tx_output: Option<TxOutput>,
    last_error: Option<String>,
}

async fn task_completed_exists(repo: &SqlxRepo, task_id: &str) -> bool {
    event_with_idem_exists(repo, "task.completed", task_id).await
}

async fn task_failed_reason(repo: &SqlxRepo, task_id: &str) -> Option<String> {
    let rows = event_payloads(repo, "task.failed").await;
    rows.into_iter()
        .find(|payload| payload["idempotency_key"] == json!(task_id))
        .and_then(|payload| payload["reason"].as_str().map(ToOwned::to_owned))
}

async fn event_with_idem_exists(repo: &SqlxRepo, kind: &str, task_id: &str) -> bool {
    event_payloads(repo, kind)
        .await
        .into_iter()
        .any(|payload| payload["idempotency_key"] == json!(task_id))
}

async fn event_payloads(repo: &SqlxRepo, kind: &str) -> Vec<Value> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(repo.pool())
            .await
            .expect("event payload rows");
    rows.into_iter()
        .map(|(payload,)| serde_json::from_str(&payload).expect("event payload json"))
        .collect()
}

fn output_string(output: &TxOutput, key: &str) -> String {
    output.data[key]
        .as_str()
        .unwrap_or_else(|| panic!("tx_output missing string field {key}: {}", output.data))
        .to_string()
}

fn e2e_budget() -> Duration {
    std::env::var("NEIGE_CODEX_FORGE_E2E_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(180))
}

fn resolve_codex_bin() -> Option<PathBuf> {
    let raw = std::env::var("NEIGE_CODEX_BIN").ok()?;
    let expanded = if let Some(stripped) = raw.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        PathBuf::from(home).join(stripped)
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_file() {
        return None;
    }
    let meta = std::fs::metadata(&expanded).ok()?;
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(expanded)
}

fn locate_shim_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("neige-mcp-stdio-shim");
    assert!(
        p.exists(),
        "neige-mcp-stdio-shim not found at {p:?}; run \
         `cargo build -p neige-mcp-stdio-shim --bin neige-mcp-stdio-shim` first, or \
         use `cargo test --workspace` which builds workspace bins",
    );
    p
}

fn seed_auth_only(home: &SharedCodexHome) {
    home.seed_from(None).expect("seed empty shared CODEX_HOME");
    let Some(host_home) = std::env::var_os("HOME") else {
        return;
    };
    let src = Path::new(&host_home).join(".codex").join("auth.json");
    if !src.exists() {
        return;
    }
    let dst = home.path().join("auth.json");
    std::fs::copy(src, dst).expect("copy host codex auth.json into test CODEX_HOME");
}

fn assert_daemon_mcp_config(home: &Path, socket_path: &Path) {
    let cfg_path = home.join("config.toml");
    let cfg_text = std::fs::read_to_string(&cfg_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", cfg_path.display()));
    assert!(
        cfg_text.contains("[mcp_servers.calm]"),
        "shared config missing mcp server block:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains("[mcp_servers.calm.env]"),
        "shared config missing mcp env block:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains("NEIGE_MCP_SOCKET"),
        "shared config missing NEIGE_MCP_SOCKET:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains("NEIGE_MCP_DAEMON_TOKEN"),
        "shared config missing NEIGE_MCP_DAEMON_TOKEN:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains(&socket_path.to_string_lossy().to_string()),
        "shared config socket does not match fixture socket:\n{cfg_text}",
    );
}

async fn preflight_initialize_through_shim(socket: &Path, daemon_token: &str) {
    let shim_bin = locate_shim_bin();
    let mut child = TokioCommand::new(&shim_bin)
        .env("NEIGE_MCP_SOCKET", socket)
        .env("NEIGE_MCP_DAEMON_TOKEN", daemon_token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn MCP shim");
    let mut stdin = child.stdin.take().expect("shim stdin");
    let stdout = child.stdout.take().expect("shim stdout");
    let init_frame = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "codex-forge-e2e", "version": "0"}
        }
    });
    stdin
        .write_all(format!("{init_frame}\n").as_bytes())
        .await
        .expect("write initialize");
    stdin.flush().await.expect("flush initialize");

    let mut reader = BufReader::new(stdout);
    let mut resp_line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut resp_line))
        .await
        .expect("preflight initialize response within 5s")
        .expect("read initialize response");
    assert!(n > 0, "MCP shim hung up before initialize response");
    let resp: Value = serde_json::from_str(resp_line.trim_end())
        .unwrap_or_else(|e| panic!("non-JSON initialize response {resp_line:?}: {e}"));
    assert!(
        resp["result"]["protocolVersion"].is_string(),
        "preflight initialize did not return protocolVersion: {resp}",
    );

    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

async fn shutdown_shared_codex(shared: &Arc<SharedCodexAppServer>) {
    let status = shared.status_snapshot();
    if let Some(runtime) = status.runtime {
        let pgid = format!("-{}", runtime.pgid);
        let _ = StdCommand::new("/bin/kill")
            .arg("-TERM")
            .arg(&pgid)
            .status();
        sleep(Duration::from_millis(200)).await;
        let _ = StdCommand::new("/bin/kill")
            .arg("-KILL")
            .arg(&pgid)
            .status();
    }
}

async fn seed_spec_session(repo: &SqlxRepo, spec_card_id: &str) {
    let mut tx = repo.pool().begin().await.expect("begin spec session tx");
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: SPEC_SESSION_ID.to_string(),
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
    .expect("seed spec session");
    tx.commit().await.expect("commit spec session tx");
}

async fn emit_workflow_registered_events_for_fixture(
    repo: &Arc<dyn Repo>,
    events: &EventBus,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    plugin_host: &Arc<PluginHost>,
) {
    let running_plugin_ids = plugin_host.running_plugin_ids().await;
    for manifest in plugin_host.registry().list() {
        let plugin_id = manifest.id.clone();
        if !running_plugin_ids.contains(&plugin_id)
            || !calm_server::forge_trust::trusted_forge_plugin(&plugin_id)
        {
            continue;
        }
        for workflow in manifest.workflows {
            repo.log_pure_event(
                ActorId::Kernel,
                EventScope::System,
                None,
                events,
                card_role_cache,
                wave_cove_cache,
                Event::WorkflowRegistered {
                    plugin_id: plugin_id.clone(),
                    workflow_id: workflow.id,
                },
            )
            .await
            .expect("log workflow.registered");
        }
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: WriteContext,
) -> Arc<PluginHost> {
    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(FORGE_BIN), bin_dir.join("git-forge"))
        .expect("symlink git-forge plugin");

    let manifest = read_manifest();
    let manifest_json = manifest.to_json();
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    repo.plugin_install(NewPlugin {
        id: PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: manifest_json,
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");

    Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        write,
    ))
}

async fn wait_for_running(host: &Arc<PluginHost>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.status(PLUGIN_ID).await
            && matches!(status.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-forge/manifest.json")
}

fn read_manifest() -> Manifest {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    Manifest::parse(&raw).expect("git-forge manifest parses")
}

fn init_bare_origin(origin: &Path, seed: &Path) {
    run_git_no_cwd(["init", "--bare", path_str(origin)]);
    std::fs::create_dir_all(seed).expect("create seed repo");
    run_git(seed, ["init"]);
    run_git(
        seed,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(seed, ["config", "user.name", "Forge Workflow Test"]);
    run_git(seed, ["branch", "-M", "main"]);
    std::fs::write(seed.join("README.md"), "initial\n").expect("write README");
    run_git(seed, ["add", "README.md"]);
    run_git(seed, ["commit", "-m", "initial"]);
    run_git(seed, ["remote", "add", "origin", path_str(origin)]);
    run_git(seed, ["push", "-u", "origin", "main"]);
    run_git_no_cwd([
        "--git-dir",
        path_str(origin),
        "symbolic-ref",
        "HEAD",
        "refs/heads/main",
    ]);
}

fn clone_for_wave(origin: &Path, target: &Path) {
    run_git_no_cwd(["clone", path_str(origin), path_str(target)]);
    configure_repo_identity(target);
}

fn configure_repo_identity(repo: &Path) {
    run_git(
        repo,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(repo, ["config", "user.name", "Forge Workflow Test"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    run_git_inner(Some(repo), args);
}

fn run_git_no_cwd<const N: usize>(args: [&str; N]) {
    run_git_inner(None, args);
}

fn run_git_capture<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = run_git_output(Some(repo), args);
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_stdout_no_cwd<const N: usize>(args: [&str; N]) -> String {
    let output = run_git_output(None, args);
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_git_inner<const N: usize>(repo: Option<&Path>, args: [&str; N]) {
    let output = run_git_output(repo, args);
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_output<const N: usize>(repo: Option<&Path>, args: [&str; N]) -> std::process::Output {
    let mut cmd = StdCommand::new("git");
    cmd.args(args);
    if let Some(repo) = repo {
        cmd.current_dir(repo);
    }
    cmd.output().expect("run git")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are utf-8")
}

fn write_gh_shim(dir: &Path) {
    let path = dir.join("gh");
    std::fs::write(&path, GH_SHIM).expect("write gh shim");
    let mut perms = std::fs::metadata(&path)
        .expect("gh shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod gh shim");
}

fn prepend_to_path(dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut value = OsString::from(dir.as_os_str());
    value.push(OsStr::new(":"));
    value.push(current);
    value
}

struct ForgeTestEnv {
    _path_dir: TempDir,
    _results_dir: TempDir,
    _trusted: EnvGuard,
    _results: EnvGuard,
    _path: EnvGuard,
}

fn setup_forge_env() -> ForgeTestEnv {
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let path = EnvGuard::set("PATH", path_value);
    ForgeTestEnv {
        _path_dir: path_dir,
        _results_dir: results_dir,
        _trusted: trusted,
        _results: results,
        _path: path,
    }
}

struct ProxyEnv {
    _http_upper: Option<EnvGuard>,
    _http_lower: Option<EnvGuard>,
    _https_upper: Option<EnvGuard>,
    _https_lower: Option<EnvGuard>,
}

fn apply_proxy_env() -> ProxyEnv {
    let proxy = active_proxy_value();
    if let Some(proxy) = proxy {
        ProxyEnv {
            _http_upper: Some(EnvGuard::set("HTTP_PROXY", &proxy)),
            _http_lower: Some(EnvGuard::set("http_proxy", &proxy)),
            _https_upper: Some(EnvGuard::set("HTTPS_PROXY", &proxy)),
            _https_lower: Some(EnvGuard::set("https_proxy", &proxy)),
        }
    } else {
        ProxyEnv {
            _http_upper: None,
            _http_lower: None,
            _https_upper: None,
            _https_lower: None,
        }
    }
}

fn active_proxy_value() -> Option<String> {
    let proxy = std::env::var("NEIGE_CODEX_PROXY").unwrap_or_else(|_| DEFAULT_PROXY.to_string());
    (!proxy.is_empty()).then_some(proxy)
}

fn short_tempdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("fwe");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

fn socket_tempdir() -> std::io::Result<TempDir> {
    let base = std::env::temp_dir().join("fwe-s");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix("s").tempdir_in(base)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn read_lossy(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| format!("<could not read {}: {e}>", path.display()))
}

const GH_SHIM: &str = r#"#!/bin/sh
get_arg() {
  wanted=$1
  shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$wanted" ]; then
      shift
      if [ "$#" -gt 0 ]; then
        printf '%s\n' "$1"
        return 0
      fi
      return 1
    fi
    shift
  done
  return 1
}

state_dir_for() {
  printf '%s.shimstate\n' "$1"
}

ensure_state() {
  repo=$1
  state=$(state_dir_for "$repo")
  mkdir -p "$state/prs" "$state/issues"
  printf '%s\n' "$state"
}

inc_counter() {
  file=$1
  if [ -f "$file" ]; then
    count=$(cat "$file")
  else
    count=0
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$file"
}

find_pr() {
  selector=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    number=$(cat "$pr_dir/number")
    head=$(cat "$pr_dir/head")
    if [ "$selector" = "$number" ] || [ "$selector" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

find_pr_by_head() {
  wanted_head=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    head=$(cat "$pr_dir/head")
    if [ "$wanted_head" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

print_pr_json() {
  pr_dir=$1
  number=$(cat "$pr_dir/number")
  head_sha=$(cat "$pr_dir/headRefOid")
  printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
}

[ "$#" -ge 2 ] || {
  echo "unsupported gh invocation" >&2
  exit 2
}

area=$1
verb=$2
shift 2

case "$area:$verb" in
  pr:list)
    repo=$(get_arg --repo "$@") || exit 2
    base=$(get_arg --base "$@" || true)
    head=$(get_arg --head "$@" || true)
    state=$(ensure_state "$repo")
    printf '['
    sep=
    for pr_dir in "$state"/prs/*; do
      [ -d "$pr_dir" ] || continue
      merged=$(cat "$pr_dir/merged")
      pr_base=$(cat "$pr_dir/base")
      pr_head=$(cat "$pr_dir/head")
      if [ "$merged" = "true" ]; then
        continue
      fi
      if [ -n "$base" ] && [ "$base" != "$pr_base" ]; then
        continue
      fi
      if [ -n "$head" ] && [ "$head" != "$pr_head" ]; then
        continue
      fi
      number=$(cat "$pr_dir/number")
      printf '%s%s' "$sep" "$number"
      sep=,
    done
    printf ']\n'
    ;;
  pr:create)
    repo=$(get_arg --repo "$@") || exit 2
    head=$(get_arg --head "$@") || exit 2
    base=$(get_arg --base "$@") || exit 2
    state=$(ensure_state "$repo")
    if pr_dir=$(find_pr_by_head "$head" "$state"); then
      print_pr_json "$pr_dir"
      exit 0
    fi
    next_file="$state/next_pr"
    if [ -f "$next_file" ]; then
      number=$(cat "$next_file")
    else
      number=1
    fi
    next=$((number + 1))
    printf '%s\n' "$next" > "$next_file"
    head_sha=$(git --git-dir "$repo" rev-parse "$head")
    pr_dir="$state/prs/$number"
    mkdir -p "$pr_dir"
    printf '%s\n' "$number" > "$pr_dir/number"
    printf '%s\n' "$head" > "$pr_dir/head"
    printf '%s\n' "$base" > "$pr_dir/base"
    printf '%s\n' "$head_sha" > "$pr_dir/headRefOid"
    printf 'false\n' > "$pr_dir/merged"
    print_pr_json "$pr_dir"
    ;;
  pr:diff)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    base=$(cat "$pr_dir/base")
    head=$(cat "$pr_dir/head")
    git --git-dir "$repo" diff --patch "$base...$head"
    ;;
  pr:view)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    number=$(cat "$pr_dir/number")
    head_sha=$(cat "$pr_dir/headRefOid")
    merged=$(cat "$pr_dir/merged")
    case "$json_fields" in
      state)
        if [ "$merged" = "true" ]; then
          printf '{"state":"MERGED"}\n'
        else
          printf '{"state":"OPEN"}\n'
        fi
        ;;
      number,headRefOid)
        printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
        ;;
      headRefOid,mergeCommit)
        if [ "$merged" = "true" ]; then
          merge_sha=$(cat "$pr_dir/merge_sha")
          printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
        else
          printf '{"headRefOid":"%s","mergeCommit":null}\n' "$head_sha"
        fi
        ;;
      statusCheckRollup)
        printf '{"conclusion":"success"}\n'
        ;;
      *)
        echo "unsupported gh pr view --json $json_fields" >&2
        exit 2
        ;;
    esac
    ;;
  pr:merge)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    expected_head=$(get_arg --match-head-commit "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    head_sha=$(cat "$pr_dir/headRefOid")
    if [ -n "$expected_head" ] && [ "$expected_head" != "$head_sha" ]; then
      echo "head commit did not match" >&2
      exit 1
    fi
    if [ "$(cat "$pr_dir/merged")" = "true" ]; then
      merge_sha=$(cat "$pr_dir/merge_sha")
    else
      number=$(cat "$pr_dir/number")
      merge_sha=$(printf '%s' "merge:$number:$head_sha" | git hash-object --stdin)
      printf '%s\n' "$merge_sha" > "$pr_dir/merge_sha"
      printf 'true\n' > "$pr_dir/merged"
      inc_counter "$state/pr_merge_count"
    fi
    printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
    ;;
  issue:view)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    jq_expr=$(get_arg --jq "$@" || true)
    state=$(ensure_state "$repo")
    issue_state=OPEN
    if [ -f "$state/issues/$issue.closed" ]; then
      issue_state=CLOSED
    fi
    case "$json_fields:$jq_expr" in
      state:*)
        printf '{"state":"%s"}\n' "$issue_state"
        ;;
      body:.body)
        printf '# Issue %s\n\nFake issue body for issue-development ingestion.\n' "$issue"
        ;;
      *)
        echo "unsupported gh issue view --json $json_fields --jq $jq_expr" >&2
        exit 2
        ;;
    esac
    ;;
  issue:close)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    if [ ! -f "$state/issues/$issue.closed" ]; then
      printf 'closed\n' > "$state/issues/$issue.closed"
      inc_counter "$state/issue_close_count"
    fi
    printf 'closed issue %s\n' "$issue"
    ;;
  *)
    echo "unsupported gh invocation: $area $verb" >&2
    exit 2
    ;;
esac
"#;
