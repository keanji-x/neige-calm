//! Host environment: the Codex binary, MCP shim, daemon config, plugin host, proxy and temp dirs.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::model::{NewPlugin, now_ms};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::shared_codex_home::SharedCodexHome;
use calm_server::state::WriteContext;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{Instant, sleep};

use super::super::forge_env::{EnvGuard, ForgeTestEnv};
use super::super::gh_shim::write_gh_shim;

use super::*;

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
        assert!(runtime.pgid > 1 && runtime.pgid != unsafe { libc::getpgrp() });
        let pgid = format!("-{}", runtime.pgid);
        let _ = StdCommand::new("/bin/kill")
            .arg("-TERM")
            .arg("--")
            .arg(&pgid)
            .status();
        sleep(Duration::from_millis(200)).await;
        let _ = StdCommand::new("/bin/kill")
            .arg("-KILL")
            .arg("--")
            .arg(&pgid)
            .status();
    }
}

pub async fn seed_planner_session(repo: &SqlxRepo, track_id: &str, planner_card_id: &str) {
    let mut tx = repo.pool().begin().await.expect("begin planner session tx");
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: PLANNER_SESSION_ID.to_string(),
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
    .expect("seed planner session");
    sqlx::query("UPDATE tracks SET root_session_id = ?1 WHERE id = ?2")
        .bind(PLANNER_SESSION_ID)
        .bind(track_id)
        .execute(&mut *tx)
        .await
        .expect("mark planner session as track root");
    tx.commit().await.expect("commit planner session tx");
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
    let registry =
        PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]).with_builtins();
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
    calm_test_sockets::try_socket_dir("s")
}

pub fn read_lossy(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| format!("<could not read {}: {e}>", path.display()))
}
