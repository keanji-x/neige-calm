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
use calm_server::model::{CardRole, NewCove, NewPlugin, NewWave, now_ms};
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
    socket_path: PathBuf,
    raw_token: String,
    thread_id: String,
    _runtime: Arc<OperationRuntime>,
    _tmp: TempDir,
}

#[derive(Clone, Copy)]
enum StubMode {
    Awaited,
    Parked,
    Malformed,
}

impl StubMode {
    fn idem_key(self) -> &'static str {
        match self {
            Self::Awaited => "stub-forge-awaited",
            Self::Parked => "stub-forge-parked",
            Self::Malformed => "stub-forge-malformed",
        }
    }

    fn parked(self) -> bool {
        matches!(self, Self::Parked)
    }

    fn malformed(self) -> bool {
        matches!(self, Self::Malformed)
    }
}

#[tokio::test]
async fn forge_action_plugin_tools_submit_await_park_and_reject_malformed() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = tempfile::tempdir().expect("forge results tempdir");
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
    awaited
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop awaited plugin");

    let parked = boot_fixture(StubMode::Parked).await;
    let parked_resp = call_forge_tool(&parked, 4).await;
    assert!(
        parked_resp.get("error").is_none(),
        "parked forge tool errored: {parked_resp:#?}"
    );
    let parked_structured = &parked_resp["result"]["structuredContent"];
    assert_eq!(parked_structured["parked"], true);
    assert!(parked_structured["op_id"].as_str().is_some());
    parked
        .plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop parked plugin");

    let malformed = boot_fixture(StubMode::Malformed).await;
    let before_ops = operation_count(&malformed.repo).await;
    let malformed_resp = call_forge_tool(&malformed, 5).await;
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

async fn boot_fixture(mode: StubMode) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path = tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
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

    let card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();
    let lease_rel = format!("leases/{card_id}");
    std::fs::create_dir_all(wave_cwd.join(&lease_rel)).expect("create lease dir");
    let mut tx = sqlx_repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave.id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint worker card");
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
    insert_workspace_lease(&mut tx, &card_id, wave.id.as_str(), &lease_rel).await;
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    let plugin_host = boot_plugin_host(
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        mode,
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
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        plugin_host_cell,
        operation_runtime_cell,
        tmp.path().join("gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    Fixture {
        _server: server,
        plugin_host,
        repo: sqlx_repo,
        socket_path,
        raw_token,
        thread_id,
        _runtime: runtime,
        _tmp: tmp,
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: calm_server::state::WriteContext,
    mode: StubMode,
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
        json!(r#"{"event_kind":"forge.scan.completed","fields":{}}"#),
    );
    env.insert(
        "STUB_FORGE_CONTEXT_JSON".into(),
        json!(r#"{"overlapping_prs":[]}"#),
    );
    env.insert("STUB_FORGE_ARGV_JSON".into(), json!(r#"["/bin/true"]"#));
    if mode.malformed() {
        env.insert("STUB_FORGE_MODE".into(), json!("malformed"));
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
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(id, EXPOSED_NAME, &fx.thread_id, json!({ "from": "test" })),
    )
    .await;
    recv_frame(&mut rd).await
}

async fn event_count(repo: &SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn operation_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(FORGE_ACTION_KIND)
        .fetch_one(repo.pool())
        .await
        .expect("operation count")
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
