//! End-to-end integration test for the Slice C `neige.*` host-callback router.
//!
//! Drives a real `PluginHost` spawning a stub plugin (`stub-plugin-caller`)
//! that issues a fixed sequence of `neige.*` callbacks. The test then asserts
//! on the kernel-side repo state — overlays exist, the new card is there, the
//! kv pair round-trips, and the rejected card was actually rejected.
//!
//! Slice C's binding spec accepted either this style or direct unit tests
//! against `callbacks::dispatch`. The unit-test alternative already lives in
//! `src/plugin_host/callbacks.rs` (covering every method with both allow and
//! deny paths). This file adds the integration spine on top so the
//! end-to-end router-task path — including the MCP frame round-trip — is
//! exercised at least once.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::{MockRepo, Repo};
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use serde_json::json;
use tokio::time::{Instant, sleep};

const CALLER_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-caller");

/// Boot a host with one caller-stub plugin installed and a cove+wave already
/// seeded in the repo. Returns the host, the repo (so the test can assert on
/// state directly), the demo wave id (also baked into the plugin's env), and
/// the tempdir guard.
async fn boot_with_wave(
    plugin_id: &str,
) -> (Arc<PluginHost>, Arc<dyn Repo>, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CALLER_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<dyn Repo> = Arc::new(MockRepo::new());
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "demo".into(),
            sort: None,
        })
        .await
        .unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Caller stub",
        "entrypoint": {
            "command": "bin/stub",
            "env": { "NEIGE_DEMO_WAVE": wave.id.clone() }
        },
        "permissions": {
            "overlays_write": ["wave", "card"],
            "cards_create": true,
            "cards_read_all": true,
            "events_subscribe": ["*"],
            "kv_quota_bytes": 1048576
        }
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir));
    let events = EventBus::new();
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        Arc::clone(&repo),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
    ));
    (host, repo, wave.id, tmp)
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

#[tokio::test]
async fn caller_stub_drives_neige_callbacks_end_to_end() {
    let plugin_id = "test.caller";
    let (host, repo, wave_id, _tmp) = boot_with_wave(plugin_id).await;

    host.spawn(plugin_id).await.expect("spawn caller stub");
    wait_for_running(&host, plugin_id).await;

    // The stub pipelines its 6 callbacks immediately after `initialize`. Each
    // call is repo-only + in-memory event emit, so the round-trip is fast.
    // Poll for the success markers with a generous budget.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let kv = repo.plugin_kv_get(plugin_id, "answer").await.unwrap();
        let cards = repo.cards_by_wave(&wave_id).await.unwrap();
        let demo_card = cards
            .iter()
            .find(|c| c.kind == format!("plugin:{plugin_id}:demo"));
        let terminal_card = cards.iter().find(|c| c.kind == "terminal");
        let other_card = cards.iter().find(|c| c.kind == "plugin:other.plugin:x");

        if let (Some(kv_val), Some(demo), Some(_term)) =
            (kv.as_ref(), demo_card, terminal_card)
        {
            // Positive assertions on what should have happened.
            assert_eq!(kv_val, &json!(42), "kv.set must roundtrip through router");

            assert_eq!(
                demo.payload,
                json!({ "hello": "world" }),
                "card payload must round-trip"
            );

            // Deny path: the plugin tried to create a card under another
            // plugin's prefix. That request must have been rejected by perms.
            assert!(
                other_card.is_none(),
                "card with other plugin's prefix must be rejected; saw: {:?}",
                other_card
            );

            // Overlay should be present and scoped to the plugin's id (server
            // enforces — the stub didn't even pass a plugin_id field).
            let overlays = repo.overlays_for("wave", &wave_id).await.unwrap();
            let our_overlay = overlays
                .iter()
                .find(|o| o.plugin_id == plugin_id && o.kind == "status")
                .expect("status overlay must be present");
            assert_eq!(our_overlay.payload, json!({ "state": "running" }));

            host.stop(plugin_id).await.ok();
            return;
        }

        if Instant::now() > deadline {
            let stderr_tail = host
                .stderr_tail(plugin_id, 50)
                .await
                .unwrap_or_default()
                .join("\n");
            panic!(
                "callbacks did not land within budget. kv={kv:?} cards={cards:?}\n--- stub stderr ---\n{stderr_tail}"
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}
