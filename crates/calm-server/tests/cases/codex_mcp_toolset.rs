//! A running-plugin-set change reaches live Codex threads (#2014): the kernel MCP entry in the
//! shared `config.toml` gets a new catalog generation and the daemon is asked to reload MCP, once
//! per burst. Driven through the real `PluginHost` lifecycle; the daemon is the fixtures fake.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::codex_mcp_toolset::{CodexMcpToolset, DEBOUNCE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::{AppContext, McpShimConfig, ToolRegistry, build_default_registry};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::shared_codex_home::{EXPECTED_MCP_SERVERS, SharedCodexHome};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");

struct Fx {
    host: Arc<PluginHost>,
    events: EventBus,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    home: Arc<SharedCodexHome>,
    appserver: Arc<SharedCodexAppServer>,
    plugins_dir: PathBuf,
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
        Some("daemon-token-hash".into()),
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
    home.ensure_daemon_mcp_config(&shim, "daemon-token")
        .unwrap();
    Fx {
        host,
        events,
        ctx,
        registry: build_default_registry(),
        home,
        appserver: SharedCodexAppServer::new_fake_running_with_pending(repo, None),
        plugins_dir,
        _tmp: tmp,
    }
}

impl Fx {
    /// The production boot entry: write the generation, then follow plugin state.
    async fn start(&self, debounce: Duration) {
        CodexMcpToolset {
            ctx: self.ctx.clone(),
            registry: self.registry.clone(),
            home: self.home.clone(),
            appserver: self.appserver.clone(),
            debounce,
        }
        .start(&self.events)
        .await;
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

    /// The config file's inode: the atomic writer renames a new file into place on every write.
    fn config_inode(&self) -> u64 {
        std::fs::metadata(self.config_path()).unwrap().ino()
    }

    fn reloads(&self) -> u64 {
        self.appserver.mcp_server_reload_count_for_test()
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

    /// Long enough for any refresh a change scheduled to have run.
    async fn settle(&self, debounce: Duration) {
        sleep(debounce * 2 + Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn enable_and_disable_each_rewrite_the_generation_and_reload_once() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.start(DEBOUNCE).await;
    let boot = fx.toolset().expect("boot writes the generation");
    assert_eq!(fx.reloads(), 0, "the boot write sends no reload");

    fx.host.enable("dev.tools").await.unwrap();
    fx.wait_reloads(1).await;
    let enabled = fx.toolset().unwrap();
    assert_ne!(enabled, boot, "a new plugin tool changes the generation");
    fx.settle(DEBOUNCE).await;
    assert_eq!(fx.reloads(), 1, "one enable is one reload");

    fx.host.disable("dev.tools").await.unwrap();
    fx.wait_reloads(2).await;
    assert_eq!(
        fx.toolset().unwrap(),
        boot,
        "the same served catalog gives the same generation"
    );
    fx.settle(DEBOUNCE).await;
    assert_eq!(fx.reloads(), 2, "one disable is one reload");
    fx.home
        .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
        .expect("the generation lives inside the kernel entry");
}

#[tokio::test]
async fn plugin_reload_rewrites_only_when_the_served_catalog_changed() {
    let fx = fixture(&[("dev.tools", &["probe.one"]), ("dev.quiet", &[])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    fx.start(DEBOUNCE).await;
    let before = fx.toolset().unwrap();
    let inode = fx.config_inode();

    // Stop and respawn with the same manifest, and enable a plugin that serves no tools: plugin
    // state changes, the served catalog does not.
    fx.host.reload("dev.tools").await.unwrap();
    fx.host.enable("dev.quiet").await.unwrap();
    fx.settle(DEBOUNCE).await;
    assert_eq!(fx.reloads(), 0, "an unchanged catalog sends no reload");
    assert_eq!(
        fx.config_inode(),
        inode,
        "an unchanged catalog writes nothing"
    );
    assert_eq!(fx.toolset().unwrap(), before);

    // A reload whose manifest now serves another tool.
    write_app(&fx.plugins_dir, "dev.tools", &["probe.one", "probe.two"]);
    fx.host.reload("dev.tools").await.unwrap();
    fx.wait_reloads(1).await;
    assert_ne!(fx.toolset().unwrap(), before);
    fx.settle(DEBOUNCE).await;
    assert_eq!(fx.reloads(), 1);
}

#[tokio::test]
async fn a_burst_of_plugin_changes_is_one_reload() {
    let debounce = Duration::from_secs(3);
    let fx = fixture(&[("dev.alpha", &["probe.a"]), ("dev.beta", &["probe.b"])]).await;
    fx.start(debounce).await;
    let boot = fx.toolset().unwrap();

    let (alpha, beta) = tokio::join!(fx.host.enable("dev.alpha"), fx.host.enable("dev.beta"));
    alpha.unwrap();
    beta.unwrap();
    fx.host.disable("dev.beta").await.unwrap();
    fx.wait_reloads(1).await;
    fx.settle(debounce).await;
    assert_eq!(fx.reloads(), 1, "the burst is gathered into one refresh");
    assert_ne!(fx.toolset().unwrap(), boot);
}

#[tokio::test]
async fn a_restart_over_an_unchanged_running_set_writes_nothing() {
    let fx = fixture(&[("dev.tools", &["probe.one"])]).await;
    fx.host.enable("dev.tools").await.unwrap();
    fx.start(DEBOUNCE).await;
    let first = fx.toolset().unwrap();
    let inode = fx.config_inode();

    // A second boot over the same home and running set.
    fx.start(DEBOUNCE).await;
    assert_eq!(fx.toolset().unwrap(), first);
    assert_eq!(fx.config_inode(), inode, "the boot write is idempotent");
    assert_eq!(fx.reloads(), 0);
}
