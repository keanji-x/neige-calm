//! Fixture boot: the shared Codex daemon, kernel, plugin host and seeded track.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::harness::HarnessRegistry;
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::{McpServer, ToolRegistry, auth, build_default_registry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::operation::forge_action_adapter::ForgeActionAdapter;
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::session_projection_repo::AgentProvider;
use calm_server::shared_codex_appserver::{SharedCodexAppServer, SharedDaemonState};
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::templates::DEV;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::TrackReportPayload;
use clap::Parser;
use serde_json::Value;
use tokio::sync::OnceCell;

use super::super::agent_diag::EvidenceTempDir;
use super::super::forge_env::EnvGuard;
use super::super::gh_shim::seed_shim_issue_body;
use super::super::git_helpers::{
    clone_for_track, git_stdout_no_cwd, init_bare_origin, point_origin_at_github,
    seed_rust_micro_crate,
};

use super::*;

pub async fn boot_real_codex_worker_fixture(codex_bin: PathBuf) -> Result<Fixture, String> {
    boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(forge_goal()),
            bound_issue: None,
            plan_source: PlanSource::Injected,
            issue_body: None,
            require_task_gates: true,
            repo_seed: RepoSeed::ReadmeOnly,
        },
        codex_bin,
    )
    .await
}

pub async fn boot_forge_e2e_fixture(
    fixture: FixtureSpec,
    codex_bin: PathBuf,
) -> Result<Fixture, String> {
    let forge_env = setup_forge_env();
    let codex_path = codex_bin
        .parent()
        .map(prepend_to_path)
        .map(|path| EnvGuard::set("PATH", path))
        .ok_or_else(|| format!("codex binary has no parent: {}", codex_bin.display()))?;
    let proxy_env = apply_proxy_env();
    // Fixtures seed events freely, so the retention pruner is pinned off.
    let events_prune_env = EnvGuard::set("NEIGE_EVENTS_PRUNE_INTERVAL_SECS", "0");

    let tmp =
        EvidenceTempDir::new(target_tmpdir("cf").map_err(|e| format!("target tempdir: {e}"))?);
    let socket_tmp = socket_tempdir().expect("MCP socket tempdir");
    let socket_path = socket_tmp.path().join("mcp").join("kernel.sock");
    calm_test_sockets::assert_fits(&socket_path);
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let track_cwd = tmp.path().join("track-cwd");
    let origin_repo = tmp.path().join("origin.git");

    match fixture.repo_seed {
        RepoSeed::ReadmeOnly => init_bare_origin(&origin_repo, &tmp.path().join("seed")),
        RepoSeed::RustMicroCrate => seed_rust_micro_crate(&origin_repo, &tmp.path().join("seed")),
    }
    clone_for_track(&origin_repo, &track_cwd);
    point_origin_at_github(&track_cwd, &origin_repo, &fixture_github_url());
    if let Some(issue) = &fixture.issue_body {
        // The gh shim keys state by the `--repo` selector string; seed both plausible selectors.
        seed_shim_issue_body(&origin_repo, issue.number, &issue.body);
        seed_shim_issue_body(&track_cwd.join(".git"), issue.number, &issue.body);
    }
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
    let track_area_cache = TrackAreaCache::new();
    let write = WriteContext::new(cache.clone(), track_area_cache.clone());
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

    let area = repo_dyn
        .area_create(NewArea {
            name: "codex-forge-e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo_dyn
        .track_create(NewTrack {
            template_input: fixture.bound_issue.map(dev_input),
            area_id: area.id.clone(),
            title: "codex-forge-e2e".into(),
            sort: None,
            cwd: track_cwd.display().to_string(),
            template_id: fixture.bound_issue.map(|_| DEV.to_string()),
            plugin_scope: Some(PLUGIN_ID.into()),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    // A codex worker runs in the track worktree (#1830 S2), as a track created by the route has.
    calm_server::test_seams::attach_track_worktree_for_test(
        sqlx_repo.pool(),
        track.id.as_str(),
        &track_cwd,
    )
    .await
    .expect("make the track worktree");
    if !fixture.require_task_gates {
        sqlx::query("UPDATE tracks SET require_task_gates = 0 WHERE id = ?1")
            .bind(track.id.as_str())
            .execute(sqlx_repo.pool())
            .await
            .expect("disable task gates for fixture track");
    }
    repo_dyn
        .seed_track_area_cache(&track_area_cache)
        .await
        .expect("seed track/area cache");
    repo_dyn
        .seed_card_role_cache(&cache)
        .await
        .expect("seed card-role cache");
    // RealPlannerTurn drives the real `planner-harness-start` op, which requires the
    // production planner card shape: kind:"codex" with the `planner_harness_card_payload` object,
    // plus the template's working method when the track is bound to one.
    let (planner_kind, planner_payload) = match (fixture.plan_source, fixture.bound_issue) {
        (PlanSource::Injected, _) => ("planner".to_string(), Value::Null),
        (PlanSource::RealPlannerTurn, None) => (
            "codex".to_string(),
            calm_server::routes::tracks::planner_harness_card_payload(
                fixture.goal.clone(),
                AgentProvider::Codex,
            ),
        ),
        (PlanSource::RealPlannerTurn, Some(_)) => (
            "codex".to_string(),
            calm_server::routes::tracks::template_planner_card_payload_for_test(
                fixture.goal.clone(),
                AgentProvider::Codex,
                DEV,
            )
            .expect("dev planner card payload"),
        ),
    };
    let planner_card = repo_dyn
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: planner_kind,
            sort: None,
            payload: planner_payload,
        })
        .await
        .expect("create planner card");
    cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    // `card_create` persists `cards.role = 'worker'` unconditionally; the report task-block
    // writer's role gate requires the planner card to carry `CardRole::Planner` as production mints it.
    super::super::mcp::set_persisted_card_role(
        repo_dyn.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    // Production `create_track` mints the track-report card for every track; this fixture
    // bypasses that route, so it mints the card here.
    let report_card = repo_dyn
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial())
                .expect("track report payload"),
        })
        .await
        .expect("create report card");
    cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    if matches!(fixture.plan_source, PlanSource::Injected) {
        seed_planner_session(&sqlx_repo, track.id.as_str(), planner_card.id.as_str()).await;
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

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    let operation_runtime_cell = Arc::new(OnceCell::new());
    let daemon_token = auth::CardMcpToken::generate().into_inner();
    let daemon_token_hash = auth::hash_token(&daemon_token);
    let server = McpServer::spawn(
        repo_dyn.clone(),
        events.clone(),
        write.clone(),
        socket_path.clone(),
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
        // that is the production calm-server port on shared boxes.
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
                    track_area_cache.clone(),
                    std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
                )) as Arc<dyn ProviderAdapter>,
                Arc::new(PlannerHarnessStartAdapter::new(
                    repo_dyn.clone(),
                    shared.clone(),
                    shared.thread_seals().clone(),
                    harness.clone(),
                    plugin_host.clone(),
                    cache.clone(),
                    track_area_cache.clone(),
                    Some(server.shim_config.socket_path.clone()),
                    std::sync::Arc::new(calm_server::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().expect("scratch claude planner host")),
                    std::sync::Arc::new(calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().expect("ACP host")),
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
        terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
        repo: route_repo,
        track_vcs: sqlx_repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events: events.clone(),
        write: write.clone(),
        daemon_token_hash: Some(daemon_token_hash),
        gate_logs_dir: tmp.path().join("gate-logs"),
        plugin_host: plugin_host_cell,
        operation_runtime: operation_runtime_cell,
        track_creator: Arc::new(tokio::sync::OnceCell::new()),
        scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
        series_resolver: Arc::new(calm_server::report_series::SeriesResolver::new_unstarted(
            None,
        )),
        plugin_results: Arc::new(calm_server::plugin_results::PluginResults::new()),
        read_ledger: Arc::new(calm_server::report_read_ledger::ReadLedger::new()),
        preview: Arc::new(calm_server::preview::PreviewRegistry::disabled()),
        sqlite_pool: sqlx_repo.sqlite_pool(),
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
        track_area_cache,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        report_card_id: report_card.id,
        codex,
        daemon,
        shared,
        runtime,
        harness,
        renderer,
        ctx,
        registry: Arc::new(registry),
        used_injected_plan: AtomicBool::new(false),
        track_cwd,
        origin_repo,
        socket_path,
        daemon_token,
        origin_main_initial,
        codex_stderr_log,
        _forge_env: forge_env,
        _codex_path: codex_path,
        _proxy_env: proxy_env,
        _events_prune_env: events_prune_env,
        _tmp: tmp,
        _socket_tmp: socket_tmp,
    })
}
