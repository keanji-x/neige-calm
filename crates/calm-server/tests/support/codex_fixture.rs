use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::harness::HarnessRegistry;
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::plan::TOOL_PLAN_UPSERT;
use calm_server::mcp_server::{McpServer, ToolRegistry, auth, build_default_registry};
use calm_server::model::{CardRole, NewCard, NewCove, NewPlugin, NewWave, now_ms};
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::operation::forge_action_adapter::ForgeActionAdapter;
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartAdapter;
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
use calm_server::wave_report::WaveReportPayload;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

use super::agent_diag::{EvidenceTempDir, panic_with_agent_diag};
use super::event_queries::*;
use super::forge_env::{EnvGuard, ForgeTestEnv};
use super::gh_shim::write_gh_shim;
use super::git_helpers::{
    clone_for_wave, git_stdout, git_stdout_no_cwd, init_bare_origin, is_hex_sha,
};
use super::spec_turn::spec_identity;

pub const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";
pub const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
pub const PLUGIN_ID: &str = "dev.neige.git-forge";
pub const COMMIT_TOOL: &str = "plugin.dev.neige.git-forge_git.commit";
pub const TASK_KEY: &str = "forge-e2e";
pub const SPEC_SESSION_ID: &str = "codex-forge-e2e-spec-session";

pub struct FixtureSpec {
    pub goal: Option<String>,
    pub workflow_id: Option<String>,
    pub plan_source: PlanSource,
    pub issue_body: Option<String>,
    pub mint_report_card: bool,
    pub require_task_gates: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSource {
    Injected,
    RealSpecTurn,
}

pub struct Fixture {
    pub server: Arc<McpServer>,
    pub plugin_host: Arc<PluginHost>,
    pub repo: Arc<SqlxRepo>,
    pub repo_dyn: Arc<dyn Repo>,
    pub events: EventBus,
    pub write: WriteContext,
    pub cache: CardRoleCache,
    pub wave_cove_cache: WaveCoveCache,
    pub cove_id: CoveId,
    pub wave_id: WaveId,
    pub spec_card_id: CardId,
    pub codex: Arc<CodexClient>,
    pub daemon: Arc<DaemonClient>,
    pub shared: Arc<SharedCodexAppServer>,
    pub runtime: Arc<OperationRuntime>,
    pub harness: HarnessRegistry,
    pub renderer: Arc<TerminalRendererRegistry>,
    pub ctx: Arc<AppContext>,
    pub registry: Arc<ToolRegistry>,
    pub used_injected_plan: AtomicBool,
    pub wave_cwd: PathBuf,
    pub origin_repo: PathBuf,
    pub origin_main_initial: String,
    pub codex_stderr_log: PathBuf,
    pub _forge_env: ForgeTestEnv,
    pub _codex_path: EnvGuard,
    pub _proxy_env: ProxyEnv,
    pub _tmp: EvidenceTempDir,
    pub _socket_tmp: TempDir,
}

impl Fixture {
    pub fn used_injected_plan(&self) -> bool {
        self.used_injected_plan.load(Ordering::SeqCst)
    }

    pub fn evidence_root(&self) -> &Path {
        self._tmp.path()
    }
}

pub fn forge_goal() -> String {
    r#"Goal: At the repository root, create a single new file named `FORGE_E2E.md` whose entire contents are exactly the single line `forge-e2e-ok`. Do not modify any other file, do not run `git push`, and do not open a pull request.
Acceptance: `FORGE_E2E.md` exists at the repository root with exactly that content."#
        .to_string()
}

pub fn forge_pr_goal(repo_gitdir: &str) -> String {
    format!(
        r#"Goal: In your current leased git worktree, perform EXACTLY these steps in order, using the MCP tools provided to you. Do not use the shell for git/gh; use the MCP tools.

1. Create a single new file named `FORGE_E2E.md` at the worktree root whose entire contents are exactly the single line `forge-e2e-ok`. Do not modify any other file.

2. Call the MCP tool whose name ends in `git.commit` (full name `plugin.dev.neige.git-forge_git.commit`) with arguments {{"message":"forge-e2e worker commit","idem":"forge-e2e-worker-commit"}} to commit that file on your current slice branch. Note the `branch` it reports.

3. Call the MCP tool whose name ends in `gh.pr.create` (full name `plugin.dev.neige.git-forge_gh.pr.create`) with arguments:
   {{"repo":"{repo}","head":"<the branch git.commit reported in step 2>","base":"main","title":"forge-e2e","body":"forge-e2e worker PR"}}
   The `repo` value MUST be exactly `{repo}`. ALL of repo, head, base, title, body are mandatory. Note the PR `number` it returns.

4. Call the MCP tool whose name ends in `gh.pr.checks` (full name `plugin.dev.neige.git-forge_gh.pr.checks`) with arguments {{"repo":"{repo}","pr":<the PR number from step 3>}}.

5. Call `calm.task.complete` with a non-empty `idempotency_key`.

Hard constraints: Do NOT run `git push`. Do NOT call gh.pr.merge. Do NOT close any issue. Do NOT call gh.pr.diff or gh.pr.list. Perform steps 2, 3, 4 in that exact order BEFORE calling calm.task.complete in step 5.
Acceptance: `FORGE_E2E.md` exists with exactly that content; a PR was created; its checks were read."#,
        repo = repo_gitdir
    )
}

pub async fn worker_operation_for_task(repo: &SqlxRepo, task_id: &str) -> Option<OperationRow> {
    operation_for_idem(repo, "codex-worker", task_id).await
}

pub async fn boot_real_codex_worker_fixture(codex_bin: PathBuf) -> Result<Fixture, String> {
    boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(forge_goal()),
            workflow_id: None,
            plan_source: PlanSource::Injected,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: true,
        },
        codex_bin,
    )
    .await
}

pub async fn boot_forge_e2e_fixture(
    spec: FixtureSpec,
    codex_bin: PathBuf,
) -> Result<Fixture, String> {
    let forge_env = setup_forge_env();
    let codex_path = codex_bin
        .parent()
        .map(prepend_to_path)
        .map(|path| EnvGuard::set("PATH", path))
        .ok_or_else(|| format!("codex binary has no parent: {}", codex_bin.display()))?;
    let proxy_env = apply_proxy_env();
    let _issue_body = spec.issue_body.as_deref();

    let tmp =
        EvidenceTempDir::new(target_tmpdir("cf").map_err(|e| format!("target tempdir: {e}"))?);
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
            workflow_id: spec.workflow_id.clone(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    if !spec.require_task_gates {
        sqlx::query("UPDATE waves SET require_task_gates = 0 WHERE id = ?1")
            .bind(wave.id.as_str())
            .execute(sqlx_repo.pool())
            .await
            .expect("disable task gates for fixture wave");
    }
    repo_dyn
        .seed_wave_cove_cache(&wave_cove_cache)
        .await
        .expect("seed wave/cove cache");
    repo_dyn
        .seed_card_role_cache(&cache)
        .await
        .expect("seed card-role cache");
    // The injected (#835) path leaves the spec card a kind:"spec"/null-payload
    // placeholder (it never boots a real harness). The RealSpecTurn path drives
    // the real `spec-harness-start` op, whose AppServerInteract phase requires
    // the production-faithful spec card shape: a kind:"codex" card whose payload
    // is the `spec_harness_card_payload` JSON object (routes/waves.rs:657) — the
    // adapter mutates that object in place (codex_thread_id, appserver_sock, …).
    let (spec_kind, spec_payload) = match spec.plan_source {
        PlanSource::Injected => ("spec".to_string(), Value::Null),
        PlanSource::RealSpecTurn => {
            let mut payload = serde_json::Map::new();
            payload.insert(
                "schemaVersion".into(),
                json!(calm_server::validation::CODEX_PAYLOAD_SCHEMA_VERSION),
            );
            payload.insert("codex_source".into(), json!("shared"));
            payload.insert("spec_harness".into(), json!(true));
            if let Some(goal) = spec
                .goal
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                payload.insert("prompt".into(), json!(goal));
            }
            ("codex".to_string(), Value::Object(payload))
        }
    };
    let spec_card = repo_dyn
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: spec_kind,
            sort: None,
            payload: spec_payload,
        })
        .await
        .expect("create spec card");
    cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    if matches!(spec.plan_source, PlanSource::RealSpecTurn) {
        // `card_create` persists `cards.role = 'worker'` unconditionally
        // (`card_create_tx` → `card_create_with_id_tx(.., CardRole::Worker, ..)`),
        // independent of kind. The injected path never reads the DB role (it
        // hand-builds `spec_identity()`), but the REAL spec turn's MCP calls
        // resolve identity via `card_identity_get_by_session`, which reads
        // `cards.role`. Production mints the spec card with
        // `card_create_with_id_tx(.., CardRole::Spec, ..)`; the test must mirror
        // that or `calm.plan.upsert` fails the role-gate with `got=Worker`.
        sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
            .bind(spec_card.id.as_str())
            .execute(sqlx_repo.pool())
            .await
            .expect("persist spec card DB role");
    }
    if spec.mint_report_card {
        let report_card = repo_dyn
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())
                    .expect("wave report payload"),
            })
            .await
            .expect("create report card");
        cache.insert(
            report_card.id.clone(),
            CardRole::ReportCard,
            wave.id.clone(),
        );
    }
    if matches!(spec.plan_source, PlanSource::Injected) {
        seed_spec_session(&sqlx_repo, spec_card.id.as_str()).await;
    }

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
        // Test codex daemons must NEVER post hooks to the default listen address —
        // that is the production calm-server port on shared boxes (production-kill
        // incident, 2026-07-04); tests do not consume hook ingest.
        "--codex-ingest-url",
        "http://127.0.0.1:1/hooks-disabled-in-e2e",
    ]);
    let home = Arc::new(SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    ));
    seed_auth_only(home.as_ref());
    home.ensure_daemon_mcp_config(&server.shim_config, &daemon_token)
        .expect("write shared daemon MCP config");
    assert_daemon_mcp_config(home.path(), &server.shim_config.socket_path);
    preflight_mcp_through_shim(&server.shim_config.socket_path, &daemon_token).await;

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
    let harness = HarnessRegistry::new();
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
                Arc::new(SpecHarnessStartAdapter::new(
                    repo_dyn.clone(),
                    shared.clone(),
                    harness.clone(),
                    plugin_host.clone(),
                    cache.clone(),
                    wave_cove_cache.clone(),
                    Some(server.shim_config.socket_path.clone()),
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
        harness,
        renderer,
        ctx,
        registry: Arc::new(registry),
        used_injected_plan: AtomicBool::new(false),
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

pub fn spawn_dispatcher(fx: &Fixture) -> Dispatcher {
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

pub async fn plan_codex_task(fx: &Fixture, key: &str, goal: &str) {
    fx.used_injected_plan.store(true, Ordering::SeqCst);
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
                "acceptance_criteria": "FORGE_E2E.md exists with exactly forge-e2e-ok",
                "no_gate_reason": "real-codex forge E2E"
            }],
            "message": "plan real codex forge worker"
        }),
    )
    .await
    .expect("plan codex task");
}

pub fn task_id(fx: &Fixture, key: &str) -> String {
    format!("{}:{key}", fx.wave_id.as_str())
}

pub async fn wait_for_worker_success(
    fx: &Fixture,
    task_id: &str,
    budget: Duration,
) -> OperationRow {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(reason) = task_failed_reason(&fx.repo, task_id).await {
            panic_with_agent_diag(
                fx,
                format!("task {task_id} failed before worker write assertion: {reason}"),
            )
            .await;
        }
        if let Some(worker) = worker_operation_for_task(&fx.repo, task_id).await {
            if worker.phase == "succeeded" {
                return worker;
            }
            if worker.phase == "failed" || worker.phase == "stuck" {
                panic_with_agent_diag(
                    fx,
                    format!(
                        "codex-worker operation for task {task_id} ended in {}",
                        worker.phase
                    ),
                )
                .await;
            }
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for codex-worker operation for task {task_id} to succeed"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn assert_worker_wrote_marker_file(fx: &Fixture, worker_cwd: &Path) {
    let marker = worker_cwd.join("FORGE_E2E.md");
    assert!(
        marker.is_file(),
        "FORGE_E2E.md was not written at {}",
        marker.display()
    );
    let contents = std::fs::read_to_string(&marker)
        .unwrap_or_else(|e| panic!("read marker file {}: {e}", marker.display()));
    assert_eq!(
        contents.trim(),
        "forge-e2e-ok",
        "FORGE_E2E.md trimmed content mismatch: {contents:?}",
    );

    let bare_main =
        git_stdout_no_cwd(["--git-dir", path_str(&fx.origin_repo), "rev-parse", "main"]);
    assert_eq!(
        bare_main, fx.origin_main_initial,
        "local bare origin main changed; worker must not push"
    );
}

pub async fn assert_worker_commit_landed(
    fx: &Fixture,
    worker_cwd: &Path,
    worker_card_id: &str,
    budget: Duration,
) {
    let row = wait_for_worktree_committed_event(fx, budget).await;
    assert_eq!(row.actor, ActorId::KernelDispatcher);
    assert_eq!(row.scope_kind, "card");
    assert_eq!(row.scope_wave.as_deref(), Some(fx.wave_id.as_str()));
    assert_eq!(row.scope_card.as_deref(), Some(worker_card_id));
    assert_eq!(row.payload["wave_id"], fx.wave_id.as_str());
    assert_eq!(row.payload["card_id"], worker_card_id);

    let head = git_stdout(worker_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head),
        "worker worktree HEAD should be a 40-char hex sha, got {head:?}"
    );
    let origin_main = git_stdout(worker_cwd, ["rev-parse", "origin/main"]);
    assert_ne!(
        head, origin_main,
        "worker worktree HEAD should diverge from origin/main after kernel commit"
    );
    assert_eq!(row.payload["commit_sha"], head);
    assert_eq!(
        row.payload["branch"],
        format!("neige/{}/{}", fx.wave_id.as_str(), worker_card_id)
    );

    let marker_at_head = git_stdout(worker_cwd, ["show", "HEAD:FORGE_E2E.md"]);
    assert_eq!(
        marker_at_head.trim(),
        "forge-e2e-ok",
        "FORGE_E2E.md content at committed HEAD mismatch"
    );
}

pub async fn wait_for_worktree_committed_event(
    fx: &Fixture,
    budget: Duration,
) -> CommittedEventRow {
    let deadline = Instant::now() + budget;
    loop {
        let rows = committed_event_rows(&fx.repo).await;
        if !rows.is_empty() {
            assert_eq!(
                rows.len(),
                1,
                "expected exactly one worktree.committed event"
            );
            return rows.into_iter().next().expect("one committed event row");
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!("timed out after {budget:?} waiting for worktree.committed"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_first_worktree_committed_event(
    fx: &Fixture,
    task_id: &str,
    budget: Duration,
) -> (i64, CommittedEventRow) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<RawCommittedEventRowWithId> = sqlx::query_as(
            "SELECT id, actor, scope_kind, scope_wave, scope_card, payload \
                 FROM events WHERE kind = 'worktree.committed' ORDER BY id ASC",
        )
        .fetch_all(fx.repo.pool())
        .await
        .expect("worktree.committed event rows");
        if let Some((id, actor, scope_kind, scope_wave, scope_card, payload)) =
            rows.into_iter().next()
        {
            return (
                id,
                CommittedEventRow {
                    actor: serde_json::from_str(&actor).expect("event actor json"),
                    scope_kind,
                    scope_wave,
                    scope_card,
                    payload: serde_json::from_str(&payload).expect("event payload json"),
                },
            );
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for first worktree.committed for task {task_id}"
                ),
            )
            .await
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_first_forge_event(
    fx: &Fixture,
    kind: &str,
    budget: Duration,
) -> (i64, Option<String>, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, Option<String>, String)> = sqlx::query_as(
            "SELECT id, scope_wave, payload FROM events WHERE kind = ?1 ORDER BY id ASC",
        )
        .bind(kind)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows: {e}"));
        if let Some((id, scope_wave, payload)) = rows.into_iter().next() {
            return (
                id,
                scope_wave,
                serde_json::from_str(&payload).expect("event payload json"),
            );
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(fx, format!("timed out after {budget:?} waiting for {kind}"))
                .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_task_completed_id(fx: &Fixture, budget: Duration) -> i64 {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM events WHERE kind = 'task.completed' ORDER BY id ASC")
                .fetch_all(fx.repo.pool())
                .await
                .expect("task.completed event rows");
        if let Some((id,)) = rows.into_iter().next() {
            return id;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!("timed out after {budget:?} waiting for task.completed"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub fn output_string(output: &TxOutput, key: &str) -> String {
    output.data[key]
        .as_str()
        .unwrap_or_else(|| panic!("tx_output missing string field {key}: {}", output.data))
        .to_string()
}

pub fn e2e_budget() -> Duration {
    std::env::var("NEIGE_CODEX_FORGE_E2E_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(180))
}

pub fn spec_planning_budget() -> Duration {
    std::env::var("NEIGE_SPEC_PLANNING_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(240))
}

pub fn resolve_codex_bin() -> Option<PathBuf> {
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

pub fn locate_shim_bin() -> PathBuf {
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

pub fn seed_auth_only(home: &SharedCodexHome) {
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

pub fn assert_daemon_mcp_config(home: &Path, socket_path: &Path) {
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

pub async fn preflight_mcp_through_shim(socket: &Path, daemon_token: &str) {
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

    let list_frame = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    stdin
        .write_all(format!("{list_frame}\n").as_bytes())
        .await
        .expect("write tools/list");
    stdin.flush().await.expect("flush tools/list");

    resp_line.clear();
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut resp_line))
        .await
        .expect("preflight tools/list response within 5s")
        .expect("read tools/list response");
    assert!(n > 0, "MCP shim hung up before tools/list response");
    let resp: Value = serde_json::from_str(resp_line.trim_end())
        .unwrap_or_else(|e| panic!("non-JSON tools/list response {resp_line:?}: {e}"));
    let tools = resp["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list missing result.tools array: {resp}"));
    let found = tools
        .iter()
        .any(|tool| tool["name"].as_str() == Some(COMMIT_TOOL));
    assert!(
        found,
        "{COMMIT_TOOL} missing from tools/list: {}",
        tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

pub async fn shutdown_shared_codex(shared: &Arc<SharedCodexAppServer>) {
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

pub async fn seed_spec_session(repo: &SqlxRepo, spec_card_id: &str) {
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

pub async fn emit_workflow_registered_events_for_fixture(
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

pub async fn boot_plugin_host(
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

pub async fn wait_for_running(host: &Arc<PluginHost>) {
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

pub fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-forge/manifest.json")
}

pub fn read_manifest() -> Manifest {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    Manifest::parse(&raw).expect("git-forge manifest parses")
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are utf-8")
}

pub fn prepend_to_path(dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut value = OsString::from(dir.as_os_str());
    value.push(OsStr::new(":"));
    value.push(current);
    value
}

pub fn setup_forge_env() -> ForgeTestEnv {
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

pub struct ProxyEnv {
    pub _http_upper: Option<EnvGuard>,
    pub _http_lower: Option<EnvGuard>,
    pub _https_upper: Option<EnvGuard>,
    pub _https_lower: Option<EnvGuard>,
}

pub fn apply_proxy_env() -> ProxyEnv {
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

pub fn active_proxy_value() -> Option<String> {
    let proxy = std::env::var("NEIGE_CODEX_PROXY").unwrap_or_else(|_| DEFAULT_PROXY.to_string());
    (!proxy.is_empty()).then_some(proxy)
}

pub fn short_tempdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("fwe");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

pub fn scratch_base() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = Path::new(&home).join(".cache").join("neige-forge-e2e");
    std::fs::create_dir_all(&base).ok()?;
    Some(base)
}

pub fn target_tmpdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = scratch_base().ok_or_else(|| std::io::Error::other("no HOME for scratch base"))?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

pub fn socket_tempdir() -> std::io::Result<TempDir> {
    let base = std::env::temp_dir().join("fwe-s");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix("s").tempdir_in(base)
}

pub fn read_lossy(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| format!("<could not read {}: {e}>", path.display()))
}
