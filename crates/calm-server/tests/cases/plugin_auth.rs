//! Per-plugin auth tokens: process token mint/rotate, auth-mismatch kill, and
//! the `experimental.dev.neige/kernel-callbacks` capability gate.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{
    Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus, hash_token, verify_token,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const CALLER_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-caller");

async fn boot_host(
    plugin_id: &str,
    extra_env: &[(&str, &str)],
) -> (Arc<PluginHost>, Arc<dyn Repo>, TempDir, EventBus) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();

    let env_map: serde_json::Map<String, Value> = extra_env
        .iter()
        .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
        .collect();
    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Auth Stub",
        "entrypoint": { "command": "bin/stub", "env": env_map }, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    // Seed the plugins row: SqlxRepo's `plugin_tokens.plugin_id` FK requires it.
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));
    (host, repo, tmp, events)
}

async fn wait_for_status(
    host: &Arc<PluginHost>,
    id: &str,
    pred: impl Fn(&PluginRuntimeStatus) -> bool,
    timeout: Duration,
) -> Option<PluginRuntimeStatus> {
    let start = Instant::now();
    loop {
        if let Some(snap) = host.status(id).await
            && pred(&snap.status)
        {
            return Some(snap.status);
        }
        if start.elapsed() > timeout {
            return host.status(id).await.map(|s| s.status);
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn process_token_persists_hash_on_spawn() {
    let (host, repo, _tmp, _events) = boot_host("test.tok1", &[]).await;
    host.spawn("test.tok1").await.expect("spawn");
    wait_for_status(
        &host,
        "test.tok1",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();

    let row = repo
        .plugin_token_get("test.tok1")
        .await
        .unwrap()
        .expect("token row");
    assert_eq!(row.0.len(), 64, "hashed token should be 64 hex chars");
    assert!(row.0.chars().all(|c| c.is_ascii_hexdigit()));

    host.stop("test.tok1").await.unwrap();
}

#[tokio::test]
async fn rotate_plugin_token_swaps_hash_and_restarts() {
    let (host, repo, _tmp, _events) = boot_host("test.tok2", &[]).await;
    host.spawn("test.tok2").await.unwrap();
    wait_for_status(
        &host,
        "test.tok2",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    let before = repo.plugin_token_get("test.tok2").await.unwrap().unwrap().0;

    host.rotate_plugin_token("test.tok2")
        .await
        .expect("rotate ok");
    wait_for_status(
        &host,
        "test.tok2",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();

    let after = repo.plugin_token_get("test.tok2").await.unwrap().unwrap().0;
    assert_ne!(before, after, "rotate must change the stored hash");

    host.stop("test.tok2").await.unwrap();
}

#[tokio::test]
async fn auth_mismatch_kills_and_does_not_respawn() {
    let (host, _repo, _tmp, _events) = boot_host(
        "test.badauth",
        &[("STUB_ECHO_OVERRIDE", "definitely-not-the-real-token")],
    )
    .await;

    let err = host
        .spawn("test.badauth")
        .await
        .expect_err("spawn should fail");
    assert!(
        matches!(err, calm_server::plugin_host::HostError::AuthMismatch(_)),
        "expected AuthMismatch, got {err:?}",
    );

    // Wait a moment to confirm no respawn supervisor fires.
    sleep(Duration::from_millis(300)).await;
    let snap = host.status("test.badauth").await;
    assert!(
        snap.is_none(),
        "no supervisor → no processes-map entry; got {:?}",
        snap.map(|s| s.status)
    );
}

#[test]
fn auth_helpers_reachable_from_public_surface() {
    let raw = "deadbeef".repeat(8); // 64 chars to mirror a real token shape
    let h = hash_token(&raw);
    assert!(verify_token(&raw, &h));
    assert!(!verify_token("nope", &h));
}

#[tokio::test]
async fn no_kernel_callbacks_capability_installs_method_not_found_drainer() {
    use calm_server::model::{NewArea, NewTrack};

    let plugin_id = "test.nocaps";

    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CALLER_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let area = repo
        .area_create(NewArea {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "demo".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    // STUB_OMIT_CAPABILITY=1 → the initialize response carries an empty `capabilities` object.
    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "No-caps caller",
        "entrypoint": {
            "command": "bin/stub",
            "env": {
                "NEIGE_DEMO_TRACK": track.id.clone(),
                "STUB_OMIT_CAPABILITY": "1"
            }
        },
        "permissions": {
            "overlays_write": ["track", "card"],
            "cards_create": true,
            "kv_quota_bytes": 1048576
        }, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    // Seed the plugins row before spawn (FK for plugin_tokens).
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));

    host.spawn(plugin_id).await.expect("spawn no-caps stub");
    wait_for_status(
        &host,
        plugin_id,
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .expect("plugin running");

    // Give the stub time to pipeline all six callbacks and the kernel time to answer each.
    sleep(Duration::from_millis(300)).await;

    let kv = repo.plugin_kv_get(plugin_id, "answer").await.unwrap();
    assert!(
        kv.is_none(),
        "neige.kv.set must NOT touch kv when capability is absent; got {kv:?}"
    );

    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert!(
        cards.is_empty(),
        "neige.card.create must NOT create cards when capability is absent; got {} cards",
        cards.len()
    );

    let overlays = repo.overlays_for("track", track.id.as_str()).await.unwrap();
    assert!(
        overlays.is_empty(),
        "neige.overlay.set must NOT write overlays when capability is absent; got {} overlays",
        overlays.len()
    );

    host.stop(plugin_id).await.expect("stop");
}
