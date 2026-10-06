//! Genuine scheduled worker delivery for forge fixtures. Candidate rows are only read here.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_mcp_token_set_tx, session_mcp_token_set_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::EventBus;
use calm_server::harness::HarnessRegistry;
use calm_server::mcp_server::{AppContext, McpServer, ToolCallIdentity, ToolRegistry};
use calm_server::model::{TaskStatus, now_ms};
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::operation::forge_action_adapter::ForgeActionAdapter;
use calm_server::operation::task_verify_adapter::TaskVerifyAdapter;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::json;

pub struct Wiring {
    pub runtime: Arc<OperationRuntime>,
    pub dispatcher: Arc<Dispatcher>,
    /// The map the dispatcher's scheduler fences child bootstraps on, handed to the state too.
    planner_recovery_locks: calm_server::per_card_lock::PerCardLocks,
    shared: Arc<SharedCodexAppServer>,
    renderer: Arc<TerminalRendererRegistry>,
    daemon: Arc<DaemonClient>,
    harness: HarnessRegistry,
}

impl Wiring {
    pub fn app_state(
        &self,
        repo: Arc<SqlxRepo>,
        ctx: Arc<AppContext>,
        plugin: Arc<calm_server::plugin_host::PluginHost>,
        cache: CardRoleCache,
        track_areas: TrackAreaCache,
        workspace: PathBuf,
    ) -> calm_server::state::AppState {
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        calm_server::state::BootState {
            repo: repo_dyn.clone(),
            workspace_root: workspace,
            workspace_root_guard: None,
            events: ctx.events.clone(),
            daemon: self.daemon.clone(),
            terminal_renderer: self.renderer.clone(),
            plugin,
            codex: Arc::new(CodexClient::new_stub()),
            db_instance_id: Arc::new(calm_server::model::new_id()),
            database_id: repo.database_id(),
            templates: calm_server::templates::TemplateRoster::builtin(),
            card_role_cache: cache,
            track_area_cache: track_areas,
            card_kind_registry: Arc::new(calm_server::card_kind::CardKindRegistry::builtins()),
            dispatcher: self.dispatcher.clone(),
            planner_recovery_locks: self.planner_recovery_locks.clone(),
            mcp_server: None,
            mcp_context: ctx.clone(),
            harness: self.harness.clone(),
            shared_codex_appserver: self.shared.clone(),
            pending_codex_threads: Arc::new(
                calm_server::pending_codex_threads::PendingThreadStartRegistry::new(
                    repo_dyn.clone(),
                    ctx.events.clone(),
                ),
            ),
            pending_codex_threads_spawn_serial: Arc::new(tokio::sync::Mutex::new(())),
            operation_runtime: self.runtime.clone(),
            worker_flow: calm_server::worker_flow::WorkerFlowDriver::from_state_parts(
                repo_dyn,
                self.shared.clone(),
                ctx.events.clone(),
            ),
            claude_planner: Arc::new(
                calm_server::claude_planner::config::ClaudePlannerHost::unconfigured_scratch()
                    .unwrap(),
            ),
            activity_wake: calm_server::track_activity::ActivityWake::detached(),
        }
        .into_app_state()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn wire(
    repo: Arc<SqlxRepo>,
    events: EventBus,
    write: WriteContext,
    cache: CardRoleCache,
    track_areas: TrackAreaCache,
    mcp: Arc<McpServer>,
    workspace: &Path,
    gate_logs: &Path,
) -> Wiring {
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo_dyn.clone(), None);
    let op_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let renderer = TerminalRendererRegistry::new_with_repo(repo_dyn.clone());
    let daemon = Arc::new(DaemonClient::new_stub());
    let spawn = SpawnCtx::new(
        repo_dyn.clone(),
        op_repo.clone(),
        daemon.clone(),
        renderer.clone(),
        events.clone(),
        completion.clone(),
    )
    .with_shared_codex_appserver(shared.clone());
    let adapters: Vec<Arc<dyn ProviderAdapter>> = vec![
        Arc::new(ForgeActionAdapter::new()),
        Arc::new(TaskVerifyAdapter::new(gate_logs.to_path_buf())),
        Arc::new(CodexWorkerAdapter::new(
            repo_dyn.clone(),
            Arc::new(CodexClient::new_stub()),
            shared.clone(),
            Some(mcp),
            cache,
            track_areas,
            workspace.to_path_buf(),
        )),
    ];
    let runtime = Arc::new(
        OperationRuntime::new(op_repo, adapters, events.clone(), completion, spawn)
            .await
            .unwrap(),
    );
    let harness = HarnessRegistry::new();
    let planner_recovery_locks = calm_server::per_card_lock::new_per_card_locks();
    let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
        repo_dyn,
        events,
        write,
        Arc::new(CodexClient::new_stub()),
        daemon.clone(),
        renderer.clone(),
        None,
        harness.clone(),
        shared.clone(),
        runtime.clone(),
        planner_recovery_locks.clone(),
        1,
        gate_logs.to_path_buf(),
    );
    Wiring {
        runtime,
        dispatcher: Arc::new(dispatcher),
        planner_recovery_locks,
        shared,
        renderer,
        daemon,
        harness,
    }
}

pub struct Started {
    pub attempt: String,
    pub card: String,
    pub token: String,
    pub thread: String,
    pub lease: String,
    pub cwd: PathBuf,
}

pub async fn planner(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    track: &str,
) -> (ToolCallIdentity, String) {
    use calm_server::db::sqlite::{session_mark_track_root_tx, session_start_runtime_tx};
    use calm_server::model::{CardRole, NewCard, new_id};
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let track_row = repo.track_get(track).await.unwrap().unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track_row.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    super::mcp::set_persisted_card_role(repo, card.id.as_str(), CardRole::Planner).await;
    cache.insert(card.id.clone(), CardRole::Planner, track_row.id.clone());
    let session = new_id();
    let thread = format!("planner-{session}");
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(thread.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    session_mark_track_root_tx(&mut tx, &track_row.id, &session.clone().into())
        .await
        .unwrap();
    card_mcp_token_set_tx(&mut tx, card.id.as_str(), &hash)
        .await
        .unwrap();
    session_mcp_token_set_tx(&mut tx, &session, &hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (
        ToolCallIdentity {
            card_id: card.id.to_string(),
            role: CardRole::Planner,
            provider: AgentProvider::Codex,
            session_id: session,
            track_id: Some(track.into()),
            area_id: track_row.area_id.to_string(),
            thread_id: thread,
        },
        token.into_inner(),
    )
}

pub fn context(
    repo: Arc<SqlxRepo>,
    events: EventBus,
    write: WriteContext,
    plugin_host: Arc<tokio::sync::OnceCell<Arc<calm_server::plugin_host::PluginHost>>>,
    operation_runtime: Arc<tokio::sync::OnceCell<Arc<OperationRuntime>>>,
    gate_logs_dir: PathBuf,
) -> Arc<AppContext> {
    Arc::new(AppContext {
        terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
        repo: repo.clone(),
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events,
        write,
        daemon_token_hash: None,
        gate_logs_dir,
        plugin_host,
        operation_runtime,
        track_creator: Arc::new(tokio::sync::OnceCell::new()),
        scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
        series_resolver: Arc::new(calm_server::report_series::SeriesResolver::new_unstarted(
            None,
        )),
        plugin_results: Arc::new(calm_server::plugin_results::PluginResults::new()),
        read_ledger: Arc::new(calm_server::report_read_ledger::ReadLedger::new()),
        preview: Arc::new(calm_server::preview::PreviewRegistry::disabled()),
        sqlite_pool: repo.sqlite_pool(),
    })
}

pub async fn start(
    repo: &SqlxRepo,
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    planner: ToolCallIdentity,
    key: &str,
) -> Started {
    let track = planner.track_id.clone().unwrap();
    super::report_writes::upsert_block(
        ctx,
        registry,
        planner,
        json!({
            "kind":"task", "payload":{
                "key":key,"kind":"codex","goal":format!("deliver {key}"),
                "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
                "ready":true,"no_gate_reason":"hermetic forge delivery fixture"
            }
        }),
    )
    .await
    .unwrap();
    let task = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(task) = repo.task_current_get(&track, key).await.unwrap() {
                assert_ne!(
                    task.status,
                    TaskStatus::Failed,
                    "{}",
                    task.status_detail.unwrap_or_default()
                );
                if task.status == TaskStatus::Running && task.worker_card_id.is_some() {
                    break task;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("scheduled delivery worker");
    let card = task.worker_card_id.unwrap();
    let (session, thread): (String, String) = sqlx::query_as("SELECT id, thread_id FROM worker_sessions WHERE card_id = ?1 ORDER BY created_at_ms DESC LIMIT 1")
        .bind(&card).fetch_one(repo.pool()).await.unwrap();
    let (lease, path): (String, String) = sqlx::query_as(
        "SELECT lease_id, path FROM workspace_leases WHERE card_id = ?1 AND state = 'held'",
    )
    .bind(&card)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card, &hash).await.unwrap();
    session_mcp_token_set_tx(&mut tx, &session, &hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Started {
        attempt: task.id,
        card,
        token: token.into_inner(),
        thread,
        lease,
        cwd: PathBuf::from(path),
    }
}

pub async fn complete(repo: &SqlxRepo, socket: &Path, started: &Started) -> String {
    let result = super::mcp::call_tool_via_socket(
        socket,
        &started.token,
        &started.thread,
        91,
        "neige_task_done",
        json!({"attempt_id":started.attempt}),
    )
    .await;
    assert!(result.get("error").is_none(), "{result:#?}");
    assert_ne!(result["result"]["isError"], true, "{result:#?}");
    wait_done_candidate(repo, &started.attempt).await
}

/// Before any other scheduler starts, prepare on a runtime whose shutdown ends every loop.
#[cfg(feature = "codex-e2e")]
pub async fn local_candidate(fx: &super::codex_fixture::Fixture) -> String {
    let repo = fx.repo.clone();
    let ctx = fx.ctx.clone();
    let registry = fx.registry.clone();
    let planner = super::planner_turn::planner_identity(fx);
    let cache = fx.cache.clone();
    let track_areas = fx.track_area_cache.clone();
    let server = fx.server.clone();
    let socket = fx.socket_path.clone();
    let workspace = fx.evidence_root().join("local-workspaces");
    let logs = fx.evidence_root().join("local-gates");
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let candidate = rt.block_on(async {
            let _wiring = wire(
                repo.clone(),
                ctx.events.clone(),
                ctx.write.clone(),
                cache,
                track_areas,
                server,
                &workspace,
                &logs,
            )
            .await;
            let started = start(&repo, &ctx, &registry, planner, "scripted-implement").await;
            super::git_helpers::stage_git_change(&started.cwd, "FORGE_E2E_D2.md", "forge-e2e-d2\n");
            complete(&repo, &socket, &started).await
        });
        rt.shutdown_timeout(Duration::from_secs(5));
        candidate
    })
    .await
    .unwrap()
}

pub async fn wait_done_candidate(repo: &SqlxRepo, attempt: &str) -> String {
    wait_done_candidate_with_budget(repo, attempt, Duration::from_secs(30)).await
}

pub async fn wait_done_candidate_with_budget(
    repo: &SqlxRepo,
    attempt: &str,
    budget: Duration,
) -> String {
    tokio::time::timeout(budget, async {
        loop {
            let row: Option<String> = sqlx::query_scalar(
                "SELECT c.commit_sha FROM task_candidates c JOIN tasks t ON t.id = c.producer_attempt_id \
                 WHERE t.id = ?1 AND t.status = 'done' AND EXISTS \
                 (SELECT 1 FROM task_git_deliveries d WHERE d.producer_attempt_id = t.id AND d.settlement = 'candidate')")
                .bind(attempt).fetch_optional(repo.pool()).await.unwrap();
            if let Some(commit) = row { return commit; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap_or_else(|_| panic!("attempt {attempt} not Done with settled candidate at {}", now_ms()))
}
