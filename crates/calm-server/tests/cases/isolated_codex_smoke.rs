//! One real scheduler/Operation/native-report loop with a runtime-owned fake provider.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::{
    isolated_codex::config::{Backend, IsolatedCodexConfig},
    mcp_server::McpServer,
    plugin_host::{PluginHost, PluginRegistry},
    state::{AppState, CodexClient, DaemonClient, WriteContext},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
#[path = "isolated_codex_fake.rs"]
mod fake;

#[test]
#[ignore = "runtime-owned fake provider child only"]
fn isolated_fake_provider() {
    fake::run();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_scheduler_native_report_retains_files_and_recording() {
    run_case("happy", calm_server::model::TaskStatus::Done).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_native_failure_stops_and_retains_files() {
    run_case("fail", calm_server::model::TaskStatus::Failed).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_completed_turn_without_report_fails() {
    run_case("no-report", calm_server::model::TaskStatus::Failed).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_lost_turn_ack_never_reissues_on_recovery() {
    run_case("lose-ack", calm_server::model::TaskStatus::Failed).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_withdrawal_stops_owned_runtime() {
    run_case("wait", calm_server::model::TaskStatus::Failed).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_boot_recovers_acknowledged_spawn_started_running() {
    run_case("crash-running", calm_server::model::TaskStatus::Done).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_codex_boot_recovers_acknowledged_spawn_started_done() {
    run_case("crash-done", calm_server::model::TaskStatus::Done).await;
}
pub(super) struct Fixture {
    pub(super) boot: crate::mcp_track_report::Boot,
    pub(super) root: tempfile::TempDir,
    pub(super) state: AppState,
    pub(super) backend: Arc<Backend>,
}

pub(super) async fn fixture(scenario: &str) -> Fixture {
    fixture_with_plugin(scenario, None).await
}

pub(super) async fn fixture_with_plugin(scenario: &str, manifest: Option<Value>) -> Fixture {
    let boot = boot().await;
    let root = tempfile::Builder::new()
        .prefix("single-loop-")
        .tempdir()
        .unwrap();
    // Capability stub for the fake provider only; actual outer namespace isolation is real.
    let sandbox_bwrap = root.path().join("sandbox-bwrap");
    std::fs::write(
        &sandbox_bwrap,
        "#!/bin/sh\nprintf '%s\\n' '--argv0 --perms --ro-bind --unshare-user --unshare-net'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sandbox_bwrap, std::fs::Permissions::from_mode(0o700)).unwrap();

    let executable = std::env::current_exe().unwrap();
    let binaries = executable.parent().unwrap().parent().unwrap();
    let helper = binaries.join("calm-worker-boundary");
    let shim = binaries.join("neige-mcp-stdio-shim");
    assert!(
        helper.is_file() && shim.is_file(),
        "build runtime and native shim in same target first"
    );
    let config = root.path().join("config.toml");
    let auth = root.path().join("auth.json");
    std::fs::write(&config, format!("model = {scenario:?}\n")).unwrap();
    std::fs::write(&auth, r#"{"tokens":{"access_token":"FAKE"}}"#).unwrap();
    let backend = Arc::new(
        Backend::with_fixture_arguments(
            IsolatedCodexConfig {
                workspace_root: root.path().join("workspaces"),
                private_root: root.path().join("private"),
                runtime_root: root.path().join("runtime"),
                runtime_helper: helper,
                runtime_bwrap: "/usr/bin/bwrap".into(),
                sandbox_bwrap,
                codex_binary: executable,
                code_mode_host_binary: std::env::current_exe().unwrap(),
                mcp_shim: shim.clone(),
                provider_config: config,
                provider_auth: auth,
                provider_environment: Default::default(),
                connect_timeout_ms: 5000,
                request_timeout_ms: 3000,
                task_timeout_ms: 30000,
            },
            vec![
                "--ignored".into(),
                "--exact".into(),
                "isolated_codex_smoke::isolated_fake_provider".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
        )
        .unwrap(),
    );
    let events = boot.ctx.events.clone();
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let write = WriteContext::new(boot.card_role_cache.clone(), areas.clone());
    let registry = if let Some(manifest) = manifest {
        let directory = root.path().join("plugins/research");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("secrets.json"),
            r#"{"key":"fixture-only-secret"}"#,
        )
        .unwrap();
        std::fs::set_permissions(
            directory.join("secrets.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        boot.repo
            .plugin_install(calm_server::model::NewPlugin {
                id: "research".into(),
                version: "0.1.0".into(),
                manifest: manifest.clone(),
                install_path: directory.display().to_string(),
                user_config: json!({}),
                enabled: true,
            })
            .await
            .unwrap();
        Arc::new(
            PluginRegistry::builder()
                .with(
                    calm_server::plugin_host::Manifest::parse(&manifest.to_string()).unwrap(),
                    Some(directory),
                )
                .build(),
        )
    } else {
        Arc::new(PluginRegistry::empty())
    };
    let plugin_host = Arc::new(PluginHost::new_full(
        registry,
        boot.repo.clone(),
        PathBuf::new(),
        root.path().join("plugins-data"),
        vec![],
        events.clone(),
        write.clone(),
    ));
    if plugin_host.registry().get("research").is_some() {
        plugin_host.spawn("research").await.unwrap();
    }
    let state = AppState::from_parts(
        boot.repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(boot.card_role_cache.clone()),
        Some(areas),
    );
    let plugins = Arc::new(tokio::sync::OnceCell::new());
    assert!(plugins.set(plugin_host).is_ok());
    let mcp = McpServer::spawn(
        boot.repo.clone(),
        events.clone(),
        write,
        root.path().join("mcp.sock"),
        shim,
        {
            let mut registry = calm_server::mcp_server::ToolRegistry::new();
            calm_server::mcp_server::tools::register_default_tools(&mut registry);
            Arc::new(registry)
        },
        None,
        plugins,
        Arc::new(tokio::sync::OnceCell::new()),
        root.path().join("gates"),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    let state = state
        .with_mcp_server(mcp)
        .with_isolated_codex_backend(backend.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    Fixture {
        boot,
        root,
        state,
        backend,
    }
}

async fn run_case(scenario: &str, expected: calm_server::model::TaskStatus) {
    let Fixture {
        boot, root, state, ..
    } = fixture(scenario).await;
    let mut published = boot.ctx.events.subscribe();
    let declaration = json!({"key":"pilot","kind":"codex","goal":"Write result.txt containing 42 and report completion through native MCP.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,
        "no_gate_reason":"Report-driven single-task fixture.","context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}});
    let (block, revision) = declare(&boot, declaration.clone()).await;
    let task = current(&boot, "pilot").await;
    let scheduler = state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        Duration::from_secs(20),
        scheduler.schedule_track(boot.track_id.clone()),
    )
    .await
    .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    let (op_id,phase,raw):(String,String,String)=sqlx::query_as("SELECT id,phase,tx_output_json FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
        .bind(&task.id).fetch_one(&pool).await.unwrap();
    assert!(
        matches!(phase.as_str(), "parked" | "succeeded" | "failed"),
        "{phase}: {}",
        current(&boot, "pilot")
            .await
            .status_detail
            .unwrap_or_default()
    );
    let record: Value = serde_json::from_str(&raw).unwrap();
    let workspace = PathBuf::from(
        record["data"]["isolated_execution"]["request"]["workspace"]
            .as_str()
            .unwrap(),
    );
    if scenario.starts_with("crash-") {
        assert_eq!(
            current(&boot, "pilot").await.status,
            calm_server::model::TaskStatus::Running
        );
        let deadline = current(&boot, "pilot").await.running_deadline_ms.unwrap();
        // Model the crash between the genuine TurnActive/TaskRunning transaction
        // and the separate outer Parked write. Keep the real receipt and runtime.
        sqlx::query("UPDATE operations SET phase='spawn_started',parked_at_ms=NULL,parked_deadline_ms=NULL,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
            .bind(&op_id).execute(&pool).await.unwrap();
        if scenario == "crash-done" {
            std::fs::write(workspace.join("report-now"), b"").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while current(&boot, "pilot").await.status != calm_server::model::TaskStatus::Done {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("real native completion before outer parking");
        }
        let recovery = state.operation_runtime.recover_on_boot().await.unwrap();
        state
            .operation_runtime
            .apply_recovery(recovery)
            .await
            .unwrap();
        let (phase, restored_deadline): (String, Option<i64>) =
            sqlx::query_as("SELECT phase,parked_deadline_ms FROM operations WHERE id=?1")
                .bind(&op_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            matches!(phase.as_str(), "parked" | "succeeded"),
            "acknowledged recovery must not fail Operation or strand Task: {phase}, task {:?}",
            current(&boot, "pilot").await.status
        );
        if scenario == "crash-running" {
            assert_eq!(
                restored_deadline,
                Some(deadline),
                "recovery must retain original running deadline"
            );
            std::fs::write(workspace.join("report-now"), b"").unwrap();
        }
    }
    if scenario == "wait" {
        assert_eq!(
            current(&boot, "pilot").await.status,
            calm_server::model::TaskStatus::Running
        );
        let mut withdrawn = declaration.clone();
        withdrawn["ready"] = json!(false);
        call_tool(
            &boot,
            calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&boot),
            json!({"id":block,"kind":"task","payload":withdrawn,"if_rev":revision}),
        )
        .await
        .unwrap();
    }
    let terminal_phase = if expected == calm_server::model::TaskStatus::Done {
        "succeeded"
    } else {
        "failed"
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
                .bind(&op_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if phase == terminal_phase {
                break;
            }
            assert_ne!(
                phase,
                if terminal_phase == "failed" {
                    "succeeded"
                } else {
                    "failed"
                },
                "{}; provider stderr: {}",
                current(&boot, "pilot")
                    .await
                    .status_detail
                    .unwrap_or_default(),
                std::fs::read_to_string(
                    root.path()
                        .join("runtime")
                        .join(&op_id)
                        .join("provider.stderr")
                )
                .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native report and exact stop must settle the parked operation");
    assert_eq!(current(&boot, "pilot").await.status, expected);
    if scenario != "lose-ack" {
        assert_eq!(
            std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
            "42\n"
        );
    }
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&op_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&raw).unwrap();
    let private = &output["data"]["isolated_execution"];
    assert!(private["provider"]["record"]["stop"]["Quiesced"].is_object());
    assert_eq!(
        private["provider"]["record"]["stop"]["Quiesced"]["handle"],
        private["provider"]["record"]["endpoint"]["boundary"]
    );
    let card = private["request"]["identity"]["card_id"].as_str().unwrap();
    if scenario != "lose-ack" {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let items = boot
                    .repo
                    .worker_flow_item_list_by_card(card, 0, 100, false)
                    .await
                    .unwrap();
                if items
                    .iter()
                    .any(|item| item.payload.contains("Created result.txt containing 42."))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("private rollout must use the existing recorder");
    }
    let public = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(
        public["tasks"][0]["status"],
        serde_json::to_value(expected).unwrap()
    );
    let recovery = state.operation_runtime.recover_on_boot().await.unwrap();
    state
        .operation_runtime
        .apply_recovery(recovery)
        .await
        .unwrap();
    assert!(!public.to_string().contains("FAKE"));
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            let envelope=published.recv().await.unwrap();
            if matches!(envelope.event,calm_server::event::Event::WorkerSessionStatusChanged {
                card_id, new_status:calm_server::session_projection_repo::WorkerSessionState::Exited,..}
                if card_id==card) {break;}
        }
    }).await.expect("actual stopped session status must be published");
    let calls = std::fs::read_to_string(
        private["provider"]["record"]["endpoint"]["home"]["home"]
            .as_str()
            .map(PathBuf::from)
            .unwrap()
            .join("fake-calls.jsonl"),
    )
    .unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "turn/start")
            .count(),
        1
    );
}
