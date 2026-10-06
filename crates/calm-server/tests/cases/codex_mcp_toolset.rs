//! A running-plugin-set change reaches live Codex threads (#2014): the kernel MCP entry in the
//! shared `config.toml` gets a new catalog generation and the daemon is asked to reload MCP, once
//! per burst. Driven through the real `PluginHost` lifecycle; the daemon is the fixtures fake.
//!
//! The follower listens on its own bus. A test runs the lifecycle operations to completion, then
//! forwards everything the host published, so each step reaches the follower as one queued burst:
//! no assumption about how fast an operation finishes against the debounce window.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::codex_mcp_toolset::{CodexMcpToolset, DEBOUNCE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, EventBus};
use calm_server::mcp_server::{
    AppContext, McpServer, McpShimConfig, ToolRegistry, build_default_registry,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::shared_codex_appserver::{FakeMcpServerReload, SharedCodexAppServer};
use calm_server::shared_codex_home::{EXPECTED_MCP_SERVERS, SharedCodexHome};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::OnceCell;
use tokio::sync::broadcast::Receiver;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const DAEMON_TOKEN: &str = "codex-mcp-toolset-daemon-token";

struct Fx {
    host: Arc<PluginHost>,
    /// Everything the plugin host published, until [`Fx::forward`] passes it on.
    host_events: std::sync::Mutex<Receiver<BroadcastEnvelope>>,
    /// The bus the follower listens on.
    followed: EventBus,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    home: Arc<SharedCodexHome>,
    appserver: Arc<SharedCodexAppServer>,
    plugins_dir: PathBuf,
    /// The kernel MCP listener a Codex thread's shim reaches, serving from `ctx` and `registry`.
    server: Arc<McpServer>,
    _tmp: TempDir,
}

/// An `app` plugin over the echo stub exposing `tools`.
fn write_app(plugins_dir: &Path, id: &str, tools: &[&str]) {
    let dir = plugins_dir.join(id);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let stub = dir.join("bin").join("stub");
    if !stub.exists() {
        std::os::unix::fs::symlink(Path::new(ECHO_BIN), stub).unwrap();
    }
    let exposes: Vec<Value> = tools
        .iter()
        .map(|name| json!({ "name": name, "description": format!("{id} {name}") }))
        .collect();
    let manifest = json!({
        "manifest_version": 1,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": id,
        "entrypoint": { "command": "bin/stub" },
        "exposes_tools": exposes,
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

/// Installed, disabled plugins `(id, tools)`, and a shared home whose kernel entry boot wrote.
async fn fixture(plugins: &[(&str, &[&str])]) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for (id, tools) in plugins {
        write_app(&plugins_dir, id, tools);
        repo.plugin_install(calm_server::model::NewPlugin {
            id: (*id).into(),
            version: "0.1.0".into(),
            install_path: plugins_dir.join(id).display().to_string(),
            manifest: json!({}),
            enabled: false,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }
    let (plugin_registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let events = EventBus::new();
    let write = calm_server::state::WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
    );
    let host = Arc::new(PluginHost::new_full(
        Arc::new(plugin_registry),
        repo.clone(),
        plugins_dir.clone(),
        data_dir.clone(),
        Vec::new(),
        events.clone(),
        write.clone(),
    ));
    let host_cell = Arc::new(OnceCell::new());
    host_cell.set(host.clone()).ok().unwrap();
    let ctx = AppContext::new(
        repo.clone(),
        events.clone(),
        write,
        Some(calm_server::mcp_server::auth::hash_token(DAEMON_TOKEN)),
        host_cell,
        Arc::new(OnceCell::new()),
        data_dir.join("gate-logs"),
    );
    let home = Arc::new(SharedCodexHome::new(
        tmp.path().join("codex-home"),
        tmp.path().join("codex-homes"),
    ));
    let shim = McpShimConfig {
        shim_bin: tmp.path().join("bin/neige-mcp-stdio-shim"),
        socket_path: tmp.path().join("mcp/kernel.sock"),
    };
    home.ensure_daemon_mcp_config(&shim, DAEMON_TOKEN).unwrap();
    let registry = build_default_registry();
    let server = McpServer::spawn_with_context(
        ctx.clone(),
        shim.socket_path.clone(),
        shim.shim_bin.clone(),
        registry.clone(),
    )
    .await
    .unwrap();
    Fx {
        host,
        host_events: std::sync::Mutex::new(events.subscribe()),
        followed: EventBus::new(),
        ctx,
        registry,
        home,
        appserver: SharedCodexAppServer::new_fake_running_with_pending(repo, None),
        plugins_dir,
        server,
        _tmp: tmp,
    }
}

impl Fx {
    /// The production boot entry: write the generation, then follow plugin state. What the host
    /// published before is history the boot write already covers, so it is dropped.
    async fn start(&self) {
        while self.host_events.lock().unwrap().try_recv().is_ok() {}
        CodexMcpToolset {
            ctx: self.ctx.clone(),
            registry: self.registry.clone(),
            home: self.home.clone(),
            appserver: self.appserver.clone(),
            debounce: DEBOUNCE,
        }
        .start(&self.followed)
        .await;
    }

    /// Pass what the host published since the last call to the follower, as one queued burst.
    fn forward(&self) {
        let mut host_events = self.host_events.lock().unwrap();
        while let Ok(envelope) = host_events.try_recv() {
            self.followed.emit_envelope_for_test(envelope);
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.path().join("config.toml")
    }

    fn toolset(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.config_path()).unwrap();
        let config: toml::Value = toml::from_str(&text).unwrap();
        config["mcp_servers"]["neige"]
            .get("env")?
            .get("NEIGE_MCP_TOOLSET")?
            .as_str()
            .map(str::to_string)
    }

    /// The catalog digest part of the generation; the counter after the dot changes on every write.
    fn digest(&self) -> String {
        let toolset = self.toolset().unwrap();
        toolset.split_once('.').unwrap().0.to_string()
    }

    /// What a new Codex thread's shim lists now: a daemon-trust `tools/list` with no thread
    /// attribution over the kernel socket, the bootstrap catalog.
    async fn bootstrap_tools(&self) -> Vec<String> {
        let stream = UnixStream::connect(&self.server.shim_config.socket_path)
            .await
            .unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd);
        rpc(
            &mut rd,
            &mut wr,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": { "name": "codex-mcp-toolset", "version": "0" },
                    "_meta": { "dev.neige/auth": { "token": DAEMON_TOKEN } }
                }
            }),
        )
        .await;
        let list = rpc(
            &mut rd,
            &mut wr,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await;
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// The config file's inode: the atomic writer renames a new file into place on every write.
    fn config_inode(&self) -> u64 {
        std::fs::metadata(self.config_path()).unwrap().ino()
    }

    fn reloads(&self) -> u64 {
        self.appserver.mcp_server_reload_count_for_test()
    }

    /// Wait until `want` reload calls reached the daemon, whatever they were answered.
    async fn wait_reload_attempts(&self, want: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.appserver.mcp_server_reload_attempt_count_for_test() < want {
            assert!(
                Instant::now() < deadline,
                "expected {want} MCP reload attempts"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_reloads(&self, want: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.reloads() < want {
            assert!(
                Instant::now() < deadline,
                "expected {want} MCP reloads, saw {}",
                self.reloads()
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    /// Long enough for any refresh a queued burst or readiness change scheduled to have run.
    async fn settle(&self) {
        sleep(DEBOUNCE * 2 + Duration::from_millis(500)).await;
    }
}

/// One JSON-RPC line round trip on the kernel socket.
async fn rpc(
    rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    frame: Value,
) -> Value {
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    wr.write_all(&bytes).await.unwrap();
    let mut line = String::new();
    rd.read_line(&mut line).await.unwrap();
    let answer: Value = serde_json::from_str(&line).unwrap();
    assert!(answer.get("error").is_none(), "{answer:#?}");
    answer
}

#[tokio::test]
async fn enable_and_disable_each_rewrite_the_generation_and_reload_once() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.start().await;
    let boot = fx.toolset().expect("boot writes the generation");
    assert_eq!(fx.reloads(), 0, "the boot write sends no reload");

    fx.host.enable("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_ne!(
        fx.digest(),
        digest_of(&boot),
        "a new plugin tool changes the digest"
    );
    fx.settle().await;
    assert_eq!(fx.reloads(), 1, "one enable is one reload");

    fx.host.disable("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reloads(2).await;
    assert_eq!(
        fx.digest(),
        digest_of(&boot),
        "the same served catalog gives the same digest"
    );
    fx.settle().await;
    assert_eq!(fx.reloads(), 2, "one disable is one reload");
    fx.home
        .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
        .expect("the generation lives inside the kernel entry");
}

#[tokio::test]
async fn a_catalog_that_changes_and_changes_back_inside_one_window_still_refreshes() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    fx.start().await;
    let before = fx.toolset().unwrap();
    let has_tool = |names: &[String]| names.iter().any(|name| name.ends_with("probe_one"));
    assert!(has_tool(&fx.bootstrap_tools().await));

    fx.host.disable("dev.tools").await.unwrap();
    // A Codex thread starting now lists, and keeps, a catalog without the plugin's tool.
    assert!(!has_tool(&fx.bootstrap_tools().await));
    fx.host.enable("dev.tools").await.unwrap();
    assert!(has_tool(&fx.bootstrap_tools().await));
    fx.forward();

    fx.wait_reloads(1).await;
    assert_eq!(
        fx.digest(),
        digest_of(&before),
        "the final catalog equals the written one"
    );
    assert_ne!(
        fx.toolset().unwrap(),
        before,
        "the entry still changes, so the reload restarts the thread's server"
    );
    fx.settle().await;
    assert_eq!(fx.reloads(), 1);
}

#[tokio::test]
async fn plugin_reload_refreshes_and_a_toolless_plugin_leaving_does_not() {
    let fx = fixture(&[("dev.tools", &["probe.one"]), ("dev.quiet", &[])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    fx.host.enable("dev.quiet").await.unwrap();
    fx.start().await;
    let before = fx.toolset().unwrap();
    let inode = fx.config_inode();

    // A plugin that serves no tools leaves Running: the served catalog never changed.
    fx.host.disable("dev.quiet").await.unwrap();
    fx.forward();
    fx.settle().await;
    assert_eq!(fx.reloads(), 0, "an unchanged catalog sends no reload");
    assert_eq!(
        fx.config_inode(),
        inode,
        "an unchanged catalog writes nothing"
    );

    // A same-manifest reload stops the plugin and brings it back: in the gap a new thread could
    // list the catalog without it, so the window that saw it enter Running forces a refresh.
    fx.host.reload("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(fx.digest(), digest_of(&before));
    assert_ne!(fx.toolset().unwrap(), before);
    fx.settle().await;
    assert_eq!(fx.reloads(), 1);

    // A reload whose manifest now serves another tool.
    write_app(&fx.plugins_dir, "dev.tools", &["probe.one", "probe.two"]);
    fx.host.reload("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reloads(2).await;
    assert_ne!(fx.digest(), digest_of(&before));
    fx.settle().await;
    assert_eq!(fx.reloads(), 2);
}

#[tokio::test]
async fn a_burst_of_plugin_changes_is_one_reload() {
    let fx = fixture(&[("dev.alpha", &["probe.a"]), ("dev.beta", &["probe.b"])]).await;
    fx.start().await;
    let boot = fx.toolset().unwrap();

    let (alpha, beta) = tokio::join!(fx.host.enable("dev.alpha"), fx.host.enable("dev.beta"));
    alpha.unwrap();
    beta.unwrap();
    fx.host.disable("dev.beta").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    fx.settle().await;
    assert_eq!(fx.reloads(), 1, "the burst is gathered into one refresh");
    assert_ne!(fx.digest(), digest_of(&boot));
    // The one refresh wrote the final catalog: a boot pass over it writes nothing.
    let inode = fx.config_inode();
    fx.start().await;
    assert_eq!(fx.config_inode(), inode);
}

#[tokio::test]
async fn each_daemon_running_sends_one_reload_for_an_adopted_daemon() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    // A restart whose running set differs from what the adopted daemon's threads listed: the boot
    // write changes the generation and sends nothing while no daemon is Running.
    fx.start().await;
    assert_eq!(fx.reloads(), 0);

    // The boot takeover installs Running.
    fx.appserver.publish_readiness_for_test(1, true);
    fx.wait_reloads(1).await;
    // The same incarnation re-stamped is not a new Running.
    fx.appserver.publish_readiness_for_test(1, true);
    fx.settle().await;
    assert_eq!(fx.reloads(), 1, "one Running is one reload");

    // A respawn: transition entry, then the next incarnation.
    fx.appserver.publish_readiness_for_test(1, false);
    fx.appserver.publish_readiness_for_test(2, true);
    fx.wait_reloads(2).await;
    fx.settle().await;
    assert_eq!(fx.reloads(), 2);
}

#[tokio::test]
async fn a_reload_owed_while_no_daemon_is_connected_is_sent_at_the_next_running() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.start().await;
    fx.appserver
        .answer_mcp_server_reload_for_test(FakeMcpServerReload::NotConnected);

    fx.host.enable("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reload_attempts(1).await;
    fx.settle().await;
    assert_eq!(fx.reloads(), 0);

    fx.appserver
        .answer_mcp_server_reload_for_test(FakeMcpServerReload::Sent);
    fx.appserver.publish_readiness_for_test(1, true);
    fx.wait_reloads(1).await;
    fx.settle().await;
    assert_eq!(fx.reloads(), 1);
}

#[tokio::test]
async fn a_failed_reload_is_retried_at_the_next_plugin_change() {
    let fx = fixture(&[("dev.tools", &["probe.one"]), ("dev.quiet", &[])]).await;
    fx.host.enable("dev.quiet").await.unwrap();
    fx.start().await;
    fx.appserver
        .answer_mcp_server_reload_for_test(FakeMcpServerReload::Fail);

    fx.host.enable("dev.tools").await.unwrap();
    fx.forward();
    fx.wait_reload_attempts(1).await;
    fx.settle().await;
    assert_eq!(fx.reloads(), 0);
    let written = fx.toolset().unwrap();

    // A plugin change that neither changes the catalog nor forces a refresh still pays the owed
    // reload: a plugin without tools leaving Running.
    fx.appserver
        .answer_mcp_server_reload_for_test(FakeMcpServerReload::Sent);
    fx.host.disable("dev.quiet").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(fx.toolset().unwrap(), written);
    fx.settle().await;
    assert_eq!(fx.reloads(), 1);
}

#[tokio::test]
async fn a_restart_over_an_unchanged_running_set_writes_nothing() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    fx.start().await;
    let first = fx.toolset().unwrap();
    let inode = fx.config_inode();

    // A second boot over the same home and running set.
    fx.start().await;
    assert_eq!(fx.toolset().unwrap(), first);
    assert_eq!(fx.config_inode(), inode, "the boot write is idempotent");
    assert_eq!(fx.reloads(), 0);
}

/// The catalog digest part of a generation `<digest>.<n>`.
fn digest_of(generation: &str) -> &str {
    generation.split_once('.').unwrap().0
}
