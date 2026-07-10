#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
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
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::{Value, json};
use support::mcp::{
    connect, handshake, recv_frame, send_frame, tools_call_frame, tools_list_frame,
};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

const TOOLCALL_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-toolcall");
const PLUGIN_ID: &str = "dev.echo";
const TOOL_NAME: &str = "do.thing";
const EXPOSED_NAME: &str = "plugin.dev.echo_do.thing";
const SECRET_NAME: &str = "plugin.dev.echo_secret";
const COLLIDING_PLUGIN_ID: &str = "dev";
const COLLIDING_TOOL_NAME: &str = "echo.do.thing";
const COLLIDING_EXPOSED_NAME: &str = "plugin.dev_echo.do.thing";

struct Fixture {
    _server: Arc<McpServer>,
    plugin_host: Arc<PluginHost>,
    socket_path: PathBuf,
    raw_token: String,
    thread_id: String,
    _tmp: TempDir,
}

#[tokio::test]
async fn worker_mcp_discovers_and_routes_colliding_dotted_plugin_tools() {
    let fx = boot_fixture().await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;

    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(list.get("error").is_none(), "tools/list errored: {list:#?}");
    let names = tool_names_from_response(&list);
    assert!(
        names.iter().any(|name| name == EXPOSED_NAME),
        "declared plugin tool missing from tools/list: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == COLLIDING_EXPOSED_NAME),
        "prefix-colliding plugin tool missing from tools/list: {names:?}"
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == EXPOSED_NAME)
            .count(),
        1,
        "dotted plugin tool must be advertised once: {names:?}"
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == COLLIDING_EXPOSED_NAME)
            .count(),
        1,
        "prefix-colliding plugin tool must be advertised once: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == SECRET_NAME),
        "undeclared plugin tool leaked into tools/list: {names:?}"
    );
    let running_ids = fx.plugin_host.running_plugin_ids().await;
    assert!(
        running_ids.contains(PLUGIN_ID),
        "fixture must have the dotted plugin running: {running_ids:?}"
    );
    assert!(
        running_ids.contains(COLLIDING_PLUGIN_ID),
        "fixture must have the prefix-colliding plugin running: {running_ids:?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(3, SECRET_NAME, &fx.thread_id, json!({ "probe": true })),
    )
    .await;
    let secret = recv_frame(&mut rd).await;
    assert_eq!(
        secret["error"]["code"], -32601,
        "undeclared plugin tool must be method-not-found, got: {secret:#?}"
    );
    assert!(
        secret["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(SECRET_NAME),
        "method-not-found should name the rejected tool: {secret:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            4,
            EXPOSED_NAME,
            &fx.thread_id,
            json!({ "payload": "from-worker" }),
        ),
    )
    .await;
    let routed = recv_frame(&mut rd).await;
    assert!(
        routed.get("error").is_none(),
        "declared plugin tool call errored: {routed:#?}"
    );
    assert_eq!(routed["result"]["isError"], false);
    assert_eq!(
        routed["result"]["structuredContent"],
        json!({
            "echo": "through-kernel",
            "tool": TOOL_NAME
        })
    );
    assert_eq!(
        routed["result"]["_meta"]["ui"]["resourceUri"],
        "ui://stub/status"
    );
    assert_eq!(
        routed["result"]["_meta"]["requested_name"], TOOL_NAME,
        "kernel must forward the stripped inner tool name to the plugin"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            5,
            COLLIDING_EXPOSED_NAME,
            &fx.thread_id,
            json!({ "payload": "from-worker-colliding" }),
        ),
    )
    .await;
    let colliding_routed = recv_frame(&mut rd).await;
    assert!(
        colliding_routed.get("error").is_none(),
        "prefix-colliding plugin tool call errored: {colliding_routed:#?}"
    );
    assert_eq!(colliding_routed["result"]["isError"], false);
    assert_eq!(
        colliding_routed["result"]["structuredContent"],
        json!({
            "echo": "through-kernel-colliding",
            "tool": COLLIDING_TOOL_NAME
        })
    );
    assert_eq!(
        colliding_routed["result"]["_meta"]["requested_name"], COLLIDING_TOOL_NAME,
        "kernel must forward the stripped inner tool name to the prefix-colliding plugin"
    );

    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
    fx.plugin_host
        .stop(COLLIDING_PLUGIN_ID)
        .await
        .expect("stop prefix-colliding plugin");
}

fn tool_names_from_response(resp: &Value) -> Vec<String> {
    let mut names = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("tool name is a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// #868: the unix socket below must fit sockaddr_un's 108-byte cap, so the
/// tempdir goes under a short base, never the repo cwd — deep
/// checkouts/worktrees overflow the cap. `env::temp_dir()` honors `TMPDIR`,
/// which can itself be deep, so fall back to literal `/tmp` when the ambient
/// base is long (same guard as `forge_merge_crash_reboot::socket_safe_tempdir`
/// and `support::codex_fixture::short_tempdir`, which is codex-e2e-gated).
fn socket_safe_tempdir() -> std::io::Result<TempDir> {
    let ambient = std::env::temp_dir();
    let base = if ambient.as_os_str().len() <= 40 {
        ambient
    } else {
        PathBuf::from("/tmp")
    };
    tempfile::Builder::new().prefix("mcpt").tempdir_in(base)
}

async fn boot_fixture() -> Fixture {
    let tmp = socket_safe_tempdir().expect("tempdir");
    let socket_path = tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");

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
            name: "mcp-plugin-tools".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "mcp-plugin-tools".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
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
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(&sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    let plugin_host = boot_plugin_host(
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir.clone(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
    )
    .await;
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host, PLUGIN_ID).await;
    plugin_host
        .spawn(COLLIDING_PLUGIN_ID)
        .await
        .expect("spawn prefix-colliding plugin");
    wait_for_running(&plugin_host, COLLIDING_PLUGIN_ID).await;

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(
        plugin_host_cell.set(plugin_host.clone()).is_ok(),
        "late-bound plugin host cell must be set once"
    );
    let server = McpServer::spawn(
        repo,
        events,
        calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        plugin_host_cell,
        Arc::new(OnceCell::new()),
        std::env::temp_dir().join("neige-test-gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    Fixture {
        _server: server,
        plugin_host,
        socket_path,
        raw_token,
        thread_id,
        _tmp: tmp,
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: calm_server::state::WriteContext,
) -> Arc<PluginHost> {
    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), bin_dir.join("stub"))
        .expect("symlink stub plugin");
    let colliding_install_dir = plugins_dir.join(COLLIDING_PLUGIN_ID);
    let colliding_bin_dir = colliding_install_dir.join("bin");
    std::fs::create_dir_all(&colliding_bin_dir).expect("create colliding plugin bin dir");
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), colliding_bin_dir.join("stub"))
        .expect("symlink colliding stub plugin");

    let manifest_json = json!({
        "manifest_version": 1,
        "id": PLUGIN_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Dotted echo",
        "entrypoint": {
            "command": "bin/stub",
            "env": {
                "STUB_TOOLCALL_MODE": "card",
                "STUB_TOOLCALL_STRUCTURED_JSON": r#"{"echo":"through-kernel","tool":"do.thing"}"#
            }
        },
        "exposes_tools": [
            { "name": TOOL_NAME, "description": "noop" }
        ],
        "permissions": {}
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let colliding_manifest_json = json!({
        "manifest_version": 1,
        "id": COLLIDING_PLUGIN_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Prefix collision",
        "entrypoint": {
            "command": "bin/stub",
            "env": {
                "STUB_TOOLCALL_MODE": "card",
                "STUB_TOOLCALL_STRUCTURED_JSON": r#"{"echo":"through-kernel-colliding","tool":"echo.do.thing"}"#
            }
        },
        "exposes_tools": [
            { "name": COLLIDING_TOOL_NAME, "description": "collides under dotted boundary" }
        ],
        "permissions": {}
    });
    let colliding_manifest: Manifest =
        Manifest::parse(&colliding_manifest_json.to_string()).expect("manifest parses");
    registry.insert(colliding_manifest, Some(colliding_install_dir.clone()));

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
    repo.plugin_install(NewPlugin {
        id: COLLIDING_PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: colliding_install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed colliding plugin row");

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

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
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
