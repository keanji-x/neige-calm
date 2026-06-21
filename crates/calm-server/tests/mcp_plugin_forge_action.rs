#![cfg(unix)]

mod support;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mcp_token_set_tx, session_projection_active_for_card_tx, session_start_runtime_tx,
};
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, CoveId, NewCove, NewPlugin, NewWave, WaveId, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use support::mcp::{
    connect, handshake, recv_frame, send_frame, tools_call_frame, tools_list_frame,
};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

const FORGE_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-forge-action");
const PLUGIN_ID: &str = "dev.neige.stub-forge";
const TOOL_NAME: &str = "forge.scan";
const EXPOSED_NAME: &str = "plugin.dev.neige.stub-forge_forge.scan";

static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct Fixture {
    _server: Arc<McpServer>,
    plugin_host: Arc<PluginHost>,
    repo: Arc<SqlxRepo>,
    card_role_cache: CardRoleCache,
    socket_path: PathBuf,
    gate_logs_dir: PathBuf,
    cove_id: String,
    raw_token: String,
    thread_id: String,
    card_id: String,
    wave_id: String,
    lease_abs: Option<PathBuf>,
    tool_call_marker: PathBuf,
    _runtime: Arc<OperationRuntime>,
    _lease_tmp: Option<TempDir>,
    _tmp: TempDir,
}

struct Caller {
    raw_token: String,
    thread_id: String,
    card_id: String,
    wave_id: String,
    lease_abs: Option<PathBuf>,
    _lease_tmp: Option<TempDir>,
}

type EventRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Value,
);
type RawEventRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

#[derive(Clone, Copy)]
enum StubMode {
    Awaited,
    AwaitFailure,
    Parked,
    Malformed,
    Override,
    Scoped,
}

impl StubMode {
    fn idem_key(self) -> &'static str {
        match self {
            Self::Awaited => "stub-forge-awaited",
            Self::AwaitFailure => "stub-forge-await-failure",
            Self::Parked => "stub-forge-parked",
            Self::Malformed => "stub-forge-malformed",
            Self::Override => "stub-forge-override",
            Self::Scoped => "stub-forge-scoped",
        }
    }

    fn parked(self) -> bool {
        matches!(self, Self::Parked)
    }

    fn stub_mode(self) -> Option<&'static str> {
        match self {
            Self::Malformed => Some("malformed"),
            Self::Override => Some("override"),
            _ => None,
        }
    }

    fn argv_json(self) -> &'static str {
        match self {
            Self::AwaitFailure => r#"["/bin/false"]"#,
            Self::Parked => r#"["/bin/sh","-c","sleep 1"]"#,
            _ => r#"["/bin/true"]"#,
        }
    }

    fn event_spec_json(self) -> &'static str {
        match self {
            Self::Scoped => r#"{"event_kind":"worktree.provisioned","fields":{}}"#,
            _ => r#"{"event_kind":"forge.scan.completed","fields":{}}"#,
        }
    }

    fn context_json(self) -> &'static str {
        match self {
            Self::Scoped => r#"{"path":"/tmp/shared-worktree"}"#,
            _ => r#"{"overlapping_prs":[]}"#,
        }
    }
}

#[tokio::test]
async fn forge_action_plugin_tools_submit_await_park_and_reject_malformed() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = short_tempdir("fr").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());

    let awaited = boot_fixture(StubMode::Awaited).await;
    assert_tool_is_discoverable(&awaited).await;
    let awaited_resp = call_forge_tool(&awaited, 3).await;
    assert!(
        awaited_resp.get("error").is_none(),
        "awaited forge tool errored: {awaited_resp:#?}"
    );
    let awaited_structured = &awaited_resp["result"]["structuredContent"];
    assert_eq!(awaited_structured["parked"], false);
    assert!(awaited_structured["op_id"].as_str().is_some());
    assert_eq!(
        awaited_structured["result"]["event_kind"],
        "forge.scan.completed"
    );
    assert_eq!(
        event_count(&awaited.repo, "forge.scan.completed").await,
        1,
        "awaited mode must persist the typed forge event"
    );
    let awaited_payload = operation_payload_by_idem(
        &awaited.repo,
        &scoped_idem_key(
            PLUGIN_ID,
            &awaited.wave_id,
            &awaited.card_id,
            StubMode::Awaited.idem_key(),
        ),
    )
    .await
    .expect("awaited op payload");
    assert_result_path_under(
        &awaited_payload,
        results_dir.path(),
        "awaited result_path must stay inside configured results dir",
    );
    awaited
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop awaited plugin");

    let failed = boot_fixture(StubMode::AwaitFailure).await;
    let failed_resp = call_forge_tool(&failed, 4).await;
    assert!(
        failed_resp.get("error").is_none(),
        "await-failure forge tool returned a JSON-RPC error: {failed_resp:#?}"
    );
    assert_eq!(failed_resp["result"]["isError"], true);
    assert!(
        failed_resp["result"]["structuredContent"]["op_id"]
            .as_str()
            .is_some()
    );
    assert!(
        failed_resp["result"]["structuredContent"]["last_error"]
            .as_str()
            .unwrap_or_default()
            .contains("forge action exited with code 1"),
        "await-mode failure must carry the operation failure: {failed_resp:#?}"
    );
    assert_eq!(
        event_count(&failed.repo, "forge.scan.completed").await,
        0,
        "failed await-mode forge action must not persist the typed event"
    );
    failed
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop failed plugin");

    let parked_worker = boot_fixture_with_role(StubMode::Parked, CardRole::Worker).await;
    let before_parked_ops = operation_count(&parked_worker.repo).await;
    let parked_worker_resp = call_forge_tool(&parked_worker, 5).await;
    assert!(
        parked_worker_resp.get("error").is_none(),
        "parked worker forge tool errored: {parked_worker_resp:#?}"
    );
    let parked_worker_structured = &parked_worker_resp["result"]["structuredContent"];
    assert_eq!(parked_worker_structured["parked"], true);
    assert!(
        parked_worker_structured["op_id"].as_str().is_some(),
        "parked worker response must carry op_id: {parked_worker_resp:#?}"
    );
    assert_eq!(
        operation_count(&parked_worker.repo).await,
        before_parked_ops + 1,
        "parked worker forge action must submit an operation"
    );
    wait_for_event_count(&parked_worker.repo, "forge.scan.completed", 1).await;
    parked_worker
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop parked worker plugin");

    let parked = boot_fixture_with_role(StubMode::Parked, CardRole::Spec).await;
    let parked_resp = call_forge_tool(&parked, 5).await;
    assert!(
        parked_resp.get("error").is_none(),
        "parked forge tool errored: {parked_resp:#?}"
    );
    let parked_structured = &parked_resp["result"]["structuredContent"];
    assert_eq!(parked_structured["parked"], true);
    assert!(parked_structured["op_id"].as_str().is_some());
    assert_eq!(
        event_count(&parked.repo, "forge.scan.completed").await,
        0,
        "parked response must return before the typed forge event lands"
    );
    wait_for_event_count(&parked.repo, "forge.scan.completed", 1).await;
    parked
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop parked plugin");

    let parked_missing_cwd = boot_fixture_with_role(StubMode::Parked, CardRole::Spec).await;
    std::fs::remove_dir_all(parked_missing_cwd._tmp.path().join("wave-cwd"))
        .expect("delete wave cwd before parked forge action");
    let parked_missing_cwd_resp = call_forge_tool(&parked_missing_cwd, 6).await;
    assert!(
        parked_missing_cwd_resp.get("error").is_none(),
        "parked missing-cwd forge tool returned a JSON-RPC error: {parked_missing_cwd_resp:#?}"
    );
    assert_eq!(parked_missing_cwd_resp["result"]["isError"], true);
    let parked_missing_cwd_structured = &parked_missing_cwd_resp["result"]["structuredContent"];
    assert!(
        parked_missing_cwd_structured["op_id"].as_str().is_some(),
        "parked already-terminal failure must carry op_id: {parked_missing_cwd_resp:#?}"
    );
    assert_ne!(
        parked_missing_cwd_structured["parked"], true,
        "parked already-terminal failure must not be reported as parked success"
    );
    assert!(
        parked_missing_cwd_structured["last_error"]
            .as_str()
            .unwrap_or_default()
            .contains("cwd_lease"),
        "parked already-terminal failure must carry the operation failure: {parked_missing_cwd_resp:#?}"
    );
    assert_eq!(
        event_count(&parked_missing_cwd.repo, "forge.scan.completed").await,
        0,
        "failed parked forge action must not persist the typed event"
    );
    parked_missing_cwd
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop parked missing-cwd plugin");

    let override_fx = boot_fixture(StubMode::Override).await;
    let override_resp = call_forge_tool(&override_fx, 7).await;
    assert!(
        override_resp.get("error").is_none(),
        "override forge tool errored: {override_resp:#?}"
    );
    let override_payload = operation_payload_by_idem(
        &override_fx.repo,
        &scoped_idem_key(
            PLUGIN_ID,
            &override_fx.wave_id,
            &override_fx.card_id,
            StubMode::Override.idem_key(),
        ),
    )
    .await
    .expect("override op payload");
    assert_eq!(override_payload["wave_id"], override_fx.wave_id);
    assert_eq!(override_payload["card_id"], override_fx.card_id);
    assert_eq!(
        PathBuf::from(
            override_payload["cwd_lease"]
                .as_str()
                .expect("cwd_lease string")
        ),
        override_fx
            .lease_abs
            .as_ref()
            .expect("override worker lease")
            .clone()
    );
    assert_ne!(override_payload["wave_id"], "attacker-wave");
    assert_ne!(override_payload["card_id"], "attacker-card");
    assert_ne!(override_payload["cwd_lease"], "/tmp/attacker-cwd-lease");
    assert_ne!(override_payload["result_path"], "/tmp/attacker.result");
    assert_result_path_under(
        &override_payload,
        results_dir.path(),
        "override result_path must be kernel-derived",
    );
    override_fx
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop override plugin");

    {
        let _untrusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", "dev.neige.other-forge");
        let untrusted = boot_fixture(StubMode::Awaited).await;
        let before_ops = operation_count(&untrusted.repo).await;
        let untrusted_resp = call_forge_tool(&untrusted, 8).await;
        assert_eq!(untrusted_resp["error"]["code"], -32602);
        assert!(
            untrusted_resp["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("plugin not trusted to submit forge actions"),
            "untrusted error should name the trust failure: {untrusted_resp:#?}"
        );
        assert_eq!(
            operation_count(&untrusted.repo).await,
            before_ops,
            "untrusted forge plugin must not submit an operation"
        );
        assert_eq!(
            event_count(&untrusted.repo, "forge.scan.completed").await,
            0,
            "untrusted forge plugin must not persist a forge event"
        );
        assert!(
            !untrusted.tool_call_marker.exists(),
            "untrusted forge plugin's tools/call handler must not be invoked"
        );
        untrusted
            .plugin_host
            .stop(PLUGIN_ID)
            .await
            .expect("stop untrusted plugin");
    }

    let malformed = boot_fixture(StubMode::Malformed).await;
    let before_ops = operation_count(&malformed.repo).await;
    let malformed_resp = call_forge_tool(&malformed, 9).await;
    assert_eq!(malformed_resp["error"]["code"], -32602);
    assert!(
        malformed_resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("forge-action plugin returned a malformed payload"),
        "malformed error should name the payload issue: {malformed_resp:#?}"
    );
    assert_eq!(
        operation_count(&malformed.repo).await,
        before_ops,
        "malformed plugin payload must not submit an operation"
    );
    assert_eq!(
        event_count(&malformed.repo, "forge.scan.completed").await,
        0,
        "malformed plugin payload must not persist a forge event"
    );
    malformed
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop malformed plugin");
}

#[tokio::test]
async fn forge_action_idempotency_is_scoped_to_kernel_caller_identity() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = short_tempdir("fr").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());

    let fx = boot_fixture(StubMode::Scoped).await;
    let probe_a = json!({ "probe_argv": ["/bin/true"] });
    let probe_b = json!({ "probe_argv": ["/bin/sh", "-c", "exit 1"] });
    let first_key = scoped_idem_key(
        PLUGIN_ID,
        &fx.wave_id,
        &fx.card_id,
        StubMode::Scoped.idem_key(),
    );
    let first_resp = call_forge_tool_with_args(&fx, 20, json!({ "probe": probe_a.clone() })).await;
    assert!(
        first_resp.get("error").is_none(),
        "first scoped forge tool errored: {first_resp:#?}"
    );
    let first_op_id = first_resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .expect("first op_id")
        .to_string();
    let first_payload = operation_payload_by_idem(&fx.repo, &first_key)
        .await
        .expect("first scoped op payload after initial submit");
    let first_result_path = first_payload["result_path"]
        .as_str()
        .expect("first result_path")
        .to_string();

    let retry_resp = call_forge_tool_with_args(&fx, 21, json!({ "probe": probe_a })).await;
    assert!(
        retry_resp.get("error").is_none(),
        "retry scoped forge tool errored: {retry_resp:#?}"
    );
    assert_eq!(
        retry_resp["result"]["structuredContent"]["op_id"]
            .as_str()
            .expect("retry op_id"),
        first_op_id,
        "same wave/card/key/payload must remain idempotent"
    );
    assert_eq!(
        operation_count(&fx.repo).await,
        1,
        "same scoped key retry must not add an operation"
    );
    assert_eq!(
        event_count(&fx.repo, "worktree.provisioned").await,
        1,
        "same scoped key retry must not replay the event"
    );
    let retry_payload = operation_payload_by_idem(&fx.repo, &first_key)
        .await
        .expect("first scoped op payload after retry");
    let retry_result_path = retry_payload["result_path"]
        .as_str()
        .expect("retry result_path");
    assert_eq!(
        retry_result_path, first_result_path,
        "same wave/card/key retry must reuse the scoped result_path"
    );

    let probe_conflict_resp = call_forge_tool_with_args(&fx, 22, json!({ "probe": probe_b })).await;
    assert!(
        probe_conflict_resp.get("error").is_none(),
        "changed-probe scoped forge tool should return an MCP tool result: {probe_conflict_resp:#?}"
    );
    assert_eq!(probe_conflict_resp["result"]["isError"], true);
    assert!(
        probe_conflict_resp["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already used with different payload"),
        "changed probe must be an idempotency payload conflict: {probe_conflict_resp:#?}"
    );
    assert_eq!(
        operation_count(&fx.repo).await,
        1,
        "changed probe with the same scoped key must not replace the first operation"
    );
    assert_eq!(
        event_count(&fx.repo, "worktree.provisioned").await,
        1,
        "changed probe conflict must not replay the event"
    );
    let after_conflict_payload = operation_payload_by_idem(&fx.repo, &first_key)
        .await
        .expect("first scoped op payload after changed-probe conflict");
    assert_eq!(
        after_conflict_payload["probe"]["probe_argv"],
        json!(["/bin/true"]),
        "changed probe conflict must leave the frozen recovery probe unchanged"
    );

    let second = create_wave_caller(&fx, CardRole::Spec).await;
    let second_resp = call_forge_tool_for_caller(&fx, &second, 23).await;
    assert!(
        second_resp.get("error").is_none(),
        "second scoped forge tool errored: {second_resp:#?}"
    );
    let second_op_id = second_resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .expect("second op_id");
    assert_ne!(
        second_op_id, first_op_id,
        "different wave/card must not reuse the first operation"
    );
    assert_eq!(
        operation_count(&fx.repo).await,
        2,
        "different wave/card with same plugin key/payload must submit a distinct operation"
    );

    let second_key = scoped_idem_key(
        PLUGIN_ID,
        &second.wave_id,
        &second.card_id,
        StubMode::Scoped.idem_key(),
    );
    assert!(
        operation_payload_by_idem(&fx.repo, StubMode::Scoped.idem_key())
            .await
            .is_none()
    );
    let first_payload = operation_payload_by_idem(&fx.repo, &first_key)
        .await
        .expect("first scoped op payload");
    let second_payload = operation_payload_by_idem(&fx.repo, &second_key)
        .await
        .expect("second scoped op payload");
    assert_eq!(first_payload["wave_id"], fx.wave_id);
    assert_eq!(first_payload["card_id"], fx.card_id);
    assert_eq!(second_payload["wave_id"], second.wave_id);
    assert_eq!(second_payload["card_id"], second.card_id);
    let stable_first_result_path = first_payload["result_path"]
        .as_str()
        .expect("stable first result_path");
    let second_result_path = second_payload["result_path"]
        .as_str()
        .expect("second result_path");
    assert_eq!(
        stable_first_result_path, first_result_path,
        "first scoped op result_path must remain stable after second submit"
    );
    assert_ne!(
        second_result_path, first_result_path,
        "different scoped operations with the same raw plugin key must not share result_path"
    );

    let rows = event_rows(&fx.repo, "worktree.provisioned").await;
    assert_eq!(rows.len(), 2, "both scoped operations must persist events");
    assert_worktree_event(&rows[0], &fx.wave_id, &fx.card_id);
    assert_worktree_event(&rows[1], &second.wave_id, &second.card_id);

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop scoped plugin");
}

#[tokio::test]
async fn forge_action_default_result_dir_is_gate_logs_sibling() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::remove("NEIGE_FORGE_RESULTS_DIR");

    let fx = boot_fixture(StubMode::Awaited).await;
    let resp = call_forge_tool(&fx, 9).await;
    assert!(
        resp.get("error").is_none(),
        "default-dir forge tool errored: {resp:#?}"
    );
    let payload = operation_payload_by_idem(
        &fx.repo,
        &scoped_idem_key(
            PLUGIN_ID,
            &fx.wave_id,
            &fx.card_id,
            StubMode::Awaited.idem_key(),
        ),
    )
    .await
    .expect("default-dir op payload");
    let expected_results_dir = fx
        .gate_logs_dir
        .parent()
        .map(|parent| parent.join("forge-results"))
        .unwrap_or_else(|| fx.gate_logs_dir.join("forge-results"));
    assert_result_path_under(
        &payload,
        &expected_results_dir,
        "default result_path must be durable beside gate logs",
    );
    assert!(
        expected_results_dir.exists(),
        "default forge results dir must be created"
    );
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop default-dir plugin");
}

async fn boot_fixture(mode: StubMode) -> Fixture {
    boot_fixture_with_role(mode, CardRole::Worker).await
}

async fn boot_fixture_with_role(mode: StubMode, role: CardRole) -> Fixture {
    let tmp = short_tempdir("mfa").expect("tempdir");
    let socket_path = tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let tool_call_marker = plugins_data_dir.join(PLUGIN_ID).join("tools-call-count");
    let wave_cwd = tmp.path().join("wave-cwd");
    std::fs::create_dir_all(&wave_cwd).expect("create wave cwd");

    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let events = EventBus::new();

    let cove = repo
        .cove_create(NewCove {
            name: "mcp-plugin-forge-action".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-plugin-forge-action".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    repo.seed_wave_cove_cache(&wave_cove_cache)
        .await
        .expect("seed wave/cove cache");

    let caller = create_card_caller(&sqlx_repo, &card_role_cache, wave.id.clone(), role).await;

    let plugin_host = boot_plugin_host(
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        mode,
        tool_call_marker.clone(),
    )
    .await;
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host).await;

    let operation_repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let runtime = Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events.clone(),
                completion,
            ),
        )
        .await
        .expect("operation runtime"),
    );

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    let operation_runtime_cell = Arc::new(OnceCell::new());
    assert!(operation_runtime_cell.set(runtime.clone()).is_ok());
    let gate_logs_dir = tmp.path().join("gate-logs");
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        plugin_host_cell,
        operation_runtime_cell,
        gate_logs_dir.clone(),
    )
    .await
    .expect("spawn McpServer");

    Fixture {
        _server: server,
        plugin_host,
        repo: sqlx_repo,
        card_role_cache,
        socket_path,
        gate_logs_dir,
        cove_id: cove.id.to_string(),
        raw_token: caller.raw_token,
        thread_id: caller.thread_id,
        card_id: caller.card_id,
        wave_id: caller.wave_id,
        lease_abs: caller.lease_abs,
        tool_call_marker,
        _runtime: runtime,
        _lease_tmp: caller._lease_tmp,
        _tmp: tmp,
    }
}

async fn create_wave_caller(fx: &Fixture, role: CardRole) -> Caller {
    let wave_cwd = fx
        ._tmp
        .path()
        .join(format!("wave-cwd-{}", calm_server::model::new_id()));
    std::fs::create_dir_all(&wave_cwd).expect("create additional wave cwd");
    let wave = fx
        .repo
        .wave_create(NewWave {
            cove_id: CoveId::from(fx.cove_id.clone()),
            title: "mcp-plugin-forge-action-extra".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create additional wave");
    create_card_caller(&fx.repo, &fx.card_role_cache, wave.id, role).await
}

async fn create_card_caller(
    sqlx_repo: &Arc<SqlxRepo>,
    card_role_cache: &CardRoleCache,
    wave_id: WaveId,
    role: CardRole,
) -> Caller {
    let card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();
    let (lease_tmp, lease_rel, lease_abs) = if role == CardRole::Worker {
        let lease_tmp = tempfile::Builder::new()
            .prefix(".forge-action-lease-")
            .tempdir_in(std::env::current_dir().expect("current dir"))
            .expect("server-cwd-relative lease tempdir");
        let lease_rel = format!(
            "{}/leases/{card_id}",
            lease_tmp
                .path()
                .file_name()
                .expect("lease tempdir basename")
                .to_string_lossy()
        );
        let lease_abs = std::env::current_dir()
            .expect("current dir")
            .join(&lease_rel);
        std::fs::create_dir_all(&lease_abs).expect("create lease dir");
        (Some(lease_tmp), Some(lease_rel), Some(lease_abs))
    } else {
        (None, None, None)
    };

    let mut tx = sqlx_repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave_id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        role,
        true,
        card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint codex card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint card MCP token");
            session_mcp_token_set_tx(&mut tx, &runtime_id, &token_hash)
                .await
                .expect("mint session MCP token");
            token.into_inner()
        }
    };
    if let Some(lease_rel) = lease_rel.as_deref() {
        insert_workspace_lease(&mut tx, &card_id, wave_id.as_str(), lease_rel).await;
    }
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    Caller {
        raw_token,
        thread_id,
        card_id,
        wave_id: wave_id.to_string(),
        lease_abs,
        _lease_tmp: lease_tmp,
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: calm_server::state::WriteContext,
    mode: StubMode,
    tool_call_marker: PathBuf,
) -> Arc<PluginHost> {
    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(FORGE_BIN), bin_dir.join("stub-forge"))
        .expect("symlink stub forge plugin");

    let mut env = serde_json::Map::new();
    env.insert("STUB_FORGE_IDEM_KEY".into(), json!(mode.idem_key()));
    env.insert("STUB_FORGE_PARKED".into(), json!(mode.parked().to_string()));
    env.insert(
        "STUB_FORGE_EVENT_SPEC_JSON".into(),
        json!(mode.event_spec_json()),
    );
    env.insert("STUB_FORGE_CONTEXT_JSON".into(), json!(mode.context_json()));
    env.insert("STUB_FORGE_ARGV_JSON".into(), json!(mode.argv_json()));
    env.insert(
        "STUB_FORGE_CALL_MARKER".into(),
        json!(tool_call_marker.display().to_string()),
    );
    if let Some(stub_mode) = mode.stub_mode() {
        env.insert("STUB_FORGE_MODE".into(), json!(stub_mode));
    }

    let manifest_json = json!({
        "manifest_version": 1,
        "id": PLUGIN_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Forge action stub",
        "entrypoint": {
            "command": "bin/stub-forge",
            "env": Value::Object(env)
        },
        "exposes_tools": [
            {
                "name": TOOL_NAME,
                "description": "submit a lowered forge scan",
                "kind": "forge-action"
            }
        ],
        "permissions": {}
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    repo.plugin_install(NewPlugin {
        id: PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
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

async fn insert_workspace_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    wave_id: &str,
    path: &str,
) {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, NULL, ?7, ?7)"#,
    )
    .bind(calm_server::model::new_id())
    .bind(card_id)
    .bind(wave_id)
    .bind(path)
    .bind("test-lease-owner")
    .bind(now + 60_000)
    .bind(now)
    .execute(&mut **tx)
    .await
    .expect("insert workspace lease");
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    let runtime_id = if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, card_id)
        .await
        .expect("active runtime lookup")
    {
        let runtime_id = runtime.id.clone();
        session_bind_attribution_tx(
            &mut tx,
            &runtime_id,
            ThreadAttribution {
                runtime_id: runtime_id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .expect("bind thread attribution");
        runtime_id
    } else {
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .expect("start runtime");
        runtime.id
    };
    tx.commit().await.expect("commit runtime tx");
    runtime_id
}

async fn wait_for_running(host: &Arc<PluginHost>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(PLUGIN_ID).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_tool_is_discoverable(fx: &Fixture) {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(list.get("error").is_none(), "tools/list errored: {list:#?}");
    let names = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&EXPOSED_NAME),
        "forge-action plugin tool missing from discovery: {names:?}"
    );
}

async fn call_forge_tool(fx: &Fixture, id: i64) -> Value {
    call_forge_tool_as(fx, &fx.raw_token, &fx.thread_id, id).await
}

async fn call_forge_tool_with_args(fx: &Fixture, id: i64, args: Value) -> Value {
    call_forge_tool_as_with_args(fx, &fx.raw_token, &fx.thread_id, id, args).await
}

async fn call_forge_tool_for_caller(fx: &Fixture, caller: &Caller, id: i64) -> Value {
    call_forge_tool_as(fx, &caller.raw_token, &caller.thread_id, id).await
}

async fn call_forge_tool_as(fx: &Fixture, raw_token: &str, thread_id: &str, id: i64) -> Value {
    call_forge_tool_as_with_args(fx, raw_token, thread_id, id, json!({ "from": "test" })).await
}

async fn call_forge_tool_as_with_args(
    fx: &Fixture,
    raw_token: &str,
    thread_id: &str,
    id: i64,
    args: Value,
) -> Value {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, raw_token).await;
    send_frame(&mut wr, tools_call_frame(id, EXPOSED_NAME, thread_id, args)).await;
    recv_frame(&mut rd).await
}

async fn event_count(repo: &SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn wait_for_event_count(repo: &SqlxRepo, kind: &str, expected: i64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = event_count(repo, kind).await;
        if count == expected {
            return;
        }
        if Instant::now() > deadline {
            panic!("expected {expected} `{kind}` events, got {count}");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn operation_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(FORGE_ACTION_KIND)
        .fetch_one(repo.pool())
        .await
        .expect("operation count")
}

async fn operation_payload_by_idem(repo: &SqlxRepo, idem_key: &str) -> Option<Value> {
    let payload: Option<String> = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind = ?1 AND idempotency_key = ?2",
    )
    .bind(FORGE_ACTION_KIND)
    .bind(idem_key)
    .fetch_optional(repo.pool())
    .await
    .expect("operation payload lookup");
    payload.map(|payload| serde_json::from_str(&payload).expect("operation payload json"))
}

fn scoped_idem_key(plugin_id: &str, wave_id: &str, card_id: &str, idem_key: &str) -> String {
    format!("{plugin_id}:{wave_id}:{card_id}:{idem_key}")
}

async fn event_rows(repo: &SqlxRepo, kind: &str) -> Vec<EventRow> {
    let rows: Vec<RawEventRow> = sqlx::query_as(
        "SELECT scope_kind, scope_cove, scope_wave, scope_card, payload \
             FROM events WHERE kind = ?1 ORDER BY id ASC",
    )
    .bind(kind)
    .fetch_all(repo.pool())
    .await
    .expect("event rows");
    rows.into_iter()
        .map(
            |(scope_kind, scope_cove, scope_wave, scope_card, payload)| {
                (
                    scope_kind,
                    scope_cove,
                    scope_wave,
                    scope_card,
                    serde_json::from_str(&payload).expect("event payload json"),
                )
            },
        )
        .collect()
}

fn assert_worktree_event(row: &EventRow, wave_id: &str, card_id: &str) {
    let (scope_kind, _scope_cove, scope_wave, scope_card, payload) = row;
    assert_eq!(scope_kind, "card");
    assert_eq!(scope_wave.as_deref(), Some(wave_id));
    assert_eq!(scope_card.as_deref(), Some(card_id));
    assert_eq!(payload["wave_id"], wave_id);
    assert_eq!(payload["card_id"], card_id);
    assert_eq!(payload["path"], "/tmp/shared-worktree");
}

fn assert_result_path_under(payload: &Value, results_dir: &Path, message: &str) {
    let result_path = PathBuf::from(payload["result_path"].as_str().expect("result_path string"));
    assert_eq!(result_path.parent(), Some(results_dir), "{message}");
    let filename = result_path
        .file_name()
        .and_then(|filename| filename.to_str())
        .expect("result_path filename");
    assert_eq!(filename.len(), 71, "{message}");
    assert!(filename.ends_with(".result"), "{message}");
}

fn short_tempdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("mfa-tmp");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
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

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
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
