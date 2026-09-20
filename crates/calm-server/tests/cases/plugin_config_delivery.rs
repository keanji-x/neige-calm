//! An `app` plugin really receives its effective configuration, and one that
//! is missing a required key really does not start.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

const CONFIG_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-config");

struct Fixture {
    host: Arc<PluginHost>,
    plugin_id: String,
    /// The concrete repo, so a test can reach past the `Repo` trait and break the store on purpose.
    repo: Arc<SqlxRepo>,
    _tmp: tempfile::TempDir,
}

/// Install one `stub-plugin-config` plugin; the returned host has not spawned it yet.
async fn boot(plugin_id: &str, config_schema: Value, user_config: Value) -> Fixture {
    boot_inner(plugin_id, config_schema, Some(user_config)).await
}

/// Same, but with no `plugins` row at all: the manifest is in the registry and the DB knows nothing about this id.
async fn boot_without_stored_row(plugin_id: &str, config_schema: Value) -> Fixture {
    boot_inner(plugin_id, config_schema, None).await
}

async fn boot_inner(plugin_id: &str, config_schema: Value, user_config: Option<Value>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CONFIG_BIN), bin_dir.join("stub")).unwrap();

    // A file DB, not `sqlite::memory:`: the unreadable-store test breaks it with bare SQL.
    let db_path = tmp.path().join("plugins.sqlite3");
    let sqlx_repo = Arc::new(
        SqlxRepo::open(&format!("sqlite://{}?mode=rwc", db_path.display()))
            .await
            .expect("open file-backed sqlite repo"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();

    let manifest_json = json!({
        "manifest_version": 3,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Config stub",
        "entrypoint": { "command": "bin/stub" },
        "config_schema": config_schema,
        "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    if let Some(user_config) = user_config {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: plugin_id.into(),
            version: "0.1.0".into(),
            install_path: install_dir.display().to_string(),
            manifest: manifest_json.clone(),
            enabled: true,
            user_config,
        })
        .await
        .expect("seed plugin row");
    }

    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        super::plugin_host_smoke::test_write_context(),
    ));

    Fixture {
        host,
        plugin_id: plugin_id.to_string(),
        repo: sqlx_repo,
        _tmp: tmp,
    }
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

/// Returns the captured `_meta["dev.neige/config"]` node verbatim (`null` if the kernel sent none).
async fn config_seen_by_plugin(fx: &Fixture) -> Value {
    let client = fx
        .host
        .mcp_client(&fx.plugin_id)
        .await
        .expect("a running app plugin has a live stdio client");
    let result = client
        .tools_call("report_config", json!({}), None)
        .await
        .expect("tools/call round trip");
    result
        .structured_content
        .and_then(|s| s.get("config_meta").cloned())
        .expect("the stub always reports a `config_meta` key")
}

fn schema_with_default() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": { "type": "string", "default": "dark" },
            "retries": { "type": "integer", "default": 3 }
        },
        "additionalProperties": false
    })
}

#[tokio::test]
async fn an_unconfigured_plugin_receives_the_manifest_defaults() {
    let fx = boot("test.cfg.defaults", schema_with_default(), json!({})).await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "theme": "dark", "retries": 3 } }),
        "the plugin must have been handed a config namespace carrying the \
         manifest's defaults"
    );
}

#[tokio::test]
async fn an_operator_value_overrides_the_default_at_the_plugin() {
    let fx = boot(
        "test.cfg.override",
        schema_with_default(),
        json!({ "theme": "light" }),
    )
    .await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "theme": "light", "retries": 3 } }),
        "the operator's value must reach the plugin, and it must not take the \
         untouched default down with it"
    );
}

/// The break is a real `DROP TABLE plugins` through the pool the host holds; this
/// read is the first time the `app` spawn path touches the DB at all.
#[tokio::test]
async fn an_unreadable_config_store_refuses_the_spawn_and_says_so() {
    let fx = boot("test.cfg.unreadable", schema_with_default(), json!({})).await;

    sqlx::query("DROP TABLE plugins")
        .execute(fx.repo.pool())
        .await
        .expect("drop the plugins table out from under the host");

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("a spawn that cannot read stored configuration must not proceed");
    assert!(
        err.to_string()
            .contains("could not read stored configuration"),
        "the refusal must name the cause: {err}"
    );

    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    assert_eq!(
        status.status.wire_name(),
        "unavailable",
        "nothing was spawned and nothing is watching, same as every other \
         pre-process refusal: got {:?}",
        status.status
    );
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");
    assert!(
        last_error.contains("could not read stored configuration"),
        "`last_error` must say the store failed, not that configuration is \
         missing: {last_error}"
    );
}

fn schema_requiring_token() -> Value {
    json!({
        "type": "object",
        "properties": {
            "token": { "type": "string" },
            "region": { "type": "string", "default": "eu" }
        },
        "required": ["token"],
        "additionalProperties": false
    })
}

#[tokio::test]
async fn a_plugin_missing_a_required_key_does_not_come_up() {
    let fx = boot("test.cfg.missing", schema_requiring_token(), json!({})).await;

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("a plugin missing a required key must not start");
    assert!(
        err.to_string().contains("token"),
        "the refusal must name the key: {err}"
    );

    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    assert_eq!(
        status.status.wire_name(),
        "unavailable",
        "§2.4's terminal state for a plugin that cannot be configured into \
         existence: got {:?}",
        status.status
    );
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");
    assert!(
        last_error.contains("missing required configuration: token"),
        "`last_error` has to say what is missing, verbatim: {last_error}"
    );
    assert!(
        !last_error.contains("region"),
        "a key satisfied by its manifest default is not missing: {last_error}"
    );
}

/// An `app` with no `plugins` row cannot complete a spawn at all (`plugin_tokens.plugin_id`
/// REFERENCES plugins), so the `Ok(None)` arm is only reachable through the refusal path.
#[tokio::test]
async fn a_plugin_with_no_stored_row_is_judged_against_its_manifest_defaults() {
    let fx = boot_without_stored_row("test.cfg.norow", schema_requiring_token()).await;

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("`token` is supplied by neither a row nor a default");
    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");

    assert!(
        last_error.contains("missing required configuration: token"),
        "a row-less plugin must be judged as configuring nothing, not as \
         unreadable: {last_error} (spawn error: {err})"
    );
    assert!(
        !last_error.contains("region"),
        "the manifest default still applies with no row in the DB, so `region` \
         is not missing: {last_error}"
    );
}

#[tokio::test]
async fn a_supplied_required_key_lets_the_plugin_start_and_arrives_with_it() {
    let fx = boot(
        "test.cfg.supplied",
        schema_requiring_token(),
        json!({ "token": "s3cret-ish" }),
    )
    .await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "token": "s3cret-ish", "region": "eu" } }),
        "the key that blocked the spawn must be the key the plugin receives"
    );
}
