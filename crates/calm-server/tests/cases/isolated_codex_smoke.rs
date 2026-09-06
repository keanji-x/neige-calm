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
    std::fs::write(&config, "model = 'happy'\n").unwrap();
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
    let state = AppState::from_parts(
        boot.repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            boot.repo.clone(),
            PathBuf::new(),
            root.path().join("plugins"),
            vec![],
            events.clone(),
            write.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(boot.card_role_cache.clone()),
        Some(areas),
    );
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
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        root.path().join("gates"),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap();
    let state = state
        .with_mcp_server(mcp)
        .with_isolated_codex_backend(backend);
    state.worker_flow.start_on_boot().await.unwrap();
    declare(&boot,json!({"key":"pilot","kind":"codex","goal":"Write result.txt containing 42 and report completion through native MCP.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,
        "no_gate_reason":"Report-driven single-task fixture.","context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})).await;
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
        matches!(phase.as_str(), "parked" | "succeeded"),
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
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
                .bind(&op_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if phase == "succeeded" {
                break;
            }
            assert_ne!(
                phase,
                "failed",
                "{}",
                current(&boot, "pilot")
                    .await
                    .status_detail
                    .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native report and exact stop must settle the parked operation");
    assert_eq!(
        current(&boot, "pilot").await.status,
        calm_server::model::TaskStatus::Done
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
        "42\n"
    );
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
    let public = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(public["tasks"][0]["status"], "done");
    assert!(!public.to_string().contains("FAKE"));
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
